//! Reading a server's files over SSH.
//!
//! By default root reaches servers through agents and holds no SSH keys for
//! the estate. An operator can choose otherwise for a server whose config and
//! logs root should read: an "SSH server" connection holds one key, in the
//! keychain, and root uses it only to run fixed read-only commands (`stat`,
//! `tail`, `find`, `journalctl`, `uptime`) with every path single-quoted. Give
//! it a user that can read what it needs and nothing more.
//!
//! The server's host key is pinned the first time root connects, in
//! `ssh_known_hosts.json` beside the config, and a different key afterwards is
//! refused: that is exactly what an intercepted connection looks like.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, ChannelMsg};

use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::files::{FileStat, Reader};
use crate::model::ServiceKind;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Where pinned host keys live: beside the config, set once at start.
static KNOWN_HOSTS: OnceLock<PathBuf> = OnceLock::new();
/// One writer at a time for the pin file.
static PIN_LOCK: Mutex<()> = Mutex::new(());

pub fn keep_known_hosts_beside(config_path: &Path) {
    let _ = KNOWN_HOSTS.set(config_path.with_file_name("ssh_known_hosts.json"));
}

fn known_hosts() -> PathBuf {
    KNOWN_HOSTS
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("root-ssh-known-hosts.json"))
}

/// A word for a POSIX shell: single-quoted, the quote itself the only thing
/// that needs care inside.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Who to connect as, where, and with which key.
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    key: Arc<PrivateKey>,
}

impl Target {
    /// From an SSH server connection: `host`, `host:port` or `user@host[:port]`
    /// as the endpoint, the username field winning over a `user@`.
    pub fn from_profile(profile: &ConnectionProfile, secret: Option<String>) -> Result<Target> {
        let (host, port, user) = address_of(&profile.endpoint, profile.username.as_deref())?;
        let pem = secret
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| AppError::Config("no private key is stored for this SSH server".into()))?;
        let key = russh::keys::decode_secret_key(&pem, None).map_err(|e| {
            AppError::Config(format!(
                "the private key could not be read ({e}); use an OpenSSH or PEM key without a passphrase"
            ))
        })?;
        Ok(Target { host, port, user, key: Arc::new(key) })
    }

    fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Host, port and user from an endpoint and a username field.
fn address_of(endpoint: &str, username: Option<&str>) -> Result<(String, u16, String)> {
    let endpoint = endpoint.trim();
    let (user_part, address) = match endpoint.rsplit_once('@') {
        Some((user, address)) => (Some(user.to_string()), address),
        None => (None, endpoint),
    };
    let (host, port) = match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            (host.to_string(), port.parse::<u16>().map_err(|_| AppError::Config(format!("`{port}` is not a port")))?)
        }
        _ => (address.to_string(), 22),
    };
    if host.is_empty() {
        return Err(AppError::Config("the SSH server needs a host".into()));
    }
    let user = username
        .map(str::to_string)
        .filter(|u| !u.trim().is_empty())
        .or(user_part)
        .ok_or_else(|| AppError::Config("the SSH server needs a username".into()))?;
    Ok((host, port, user))
}

/// Check a host key against the one pinned for this host, pinning it the
/// first time. A pin that cannot be written refuses the connection, since an
/// unrecorded pin is no pin at all.
fn admit(file: &Path, host: &str, fingerprint: &str) -> std::result::Result<(), String> {
    let _one_writer = PIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut pins: BTreeMap<String, String> = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    match pins.get(host) {
        Some(pinned) if pinned == fingerprint => Ok(()),
        Some(pinned) => Err(format!(
            "{host} presented host key {fingerprint}, but {pinned} was pinned when root first connected. \
             If the server was rebuilt, remove its line from {}; if not, something is in the way.",
            file.display()
        )),
        None => {
            pins.insert(host.to_string(), fingerprint.to_string());
            let text = serde_json::to_string_pretty(&pins).unwrap_or_default();
            std::fs::write(file, text).map_err(|e| format!("could not record {host}'s host key in {}: {e}", file.display()))
        }
    }
}

struct Pin {
    host: String,
    file: PathBuf,
    refusal: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Pin {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let fingerprint = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256),
            PublicKeyOrCertificate::Certificate(cert) => cert.public_key().fingerprint(HashAlg::Sha256),
        }
        .to_string();
        match admit(&self.file, &self.host, &fingerprint) {
            Ok(()) => Ok(true),
            Err(why) => {
                *self.refusal.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                Ok(false)
            }
        }
    }
}

/// One authenticated connection, for a poll's worth of commands.
pub struct Session {
    handle: client::Handle<Pin>,
    place: String,
}

impl Session {
    /// Connect, check the host key against `pins`, and authenticate.
    pub async fn open(target: &Target, pins: &Path) -> Result<Session> {
        let fail = |e: String| AppError::Collector(format!("SSH to {}: {e}", target.address()));
        let refusal = Arc::new(Mutex::new(None));
        let pin = Pin { host: target.address(), file: pins.to_path_buf(), refusal: Arc::clone(&refusal) };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(COMMAND_TIMEOUT),
            ..Default::default()
        });
        let connecting = client::connect(config, (target.host.as_str(), target.port), pin);
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Err(_) => return Err(fail(format!("no answer within {} s", CONNECT_TIMEOUT.as_secs()))),
            Ok(Err(e)) => {
                let why = refusal.lock().unwrap_or_else(|e| e.into_inner()).take();
                return Err(fail(why.unwrap_or_else(|| e.to_string())));
            }
            Ok(Ok(handle)) => handle,
        };
        let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
        let auth = handle
            .authenticate_publickey(target.user.clone(), PrivateKeyWithHashAlg::new(Arc::clone(&target.key), hash))
            .await
            .map_err(|e| fail(e.to_string()))?;
        if !auth.success() {
            return Err(fail(format!("the server did not accept the key for {}", target.user)));
        }
        Ok(Session { handle, place: format!("{}@{}", target.user, target.host) })
    }

    /// Run one command and return its output, at most `cap` bytes of it. A
    /// non-zero exit is an error carrying what the command said.
    pub async fn run(&self, command: &str, cap: usize) -> Result<Vec<u8>> {
        let fail = |e: String| AppError::Collector(format!("{} on {}: {e}", command.split(' ').next().unwrap_or(command), self.place));
        let work = async {
            let mut channel = self.handle.channel_open_session().await.map_err(|e| fail(e.to_string()))?;
            channel.exec(true, command).await.map_err(|e| fail(e.to_string()))?;
            let (mut out, mut err, mut status) = (Vec::new(), Vec::new(), None);
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        let room = cap.saturating_sub(out.len());
                        out.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    ChannelMsg::ExtendedData { data, .. } if err.len() < 2048 => err.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    _ => {}
                }
            }
            match status {
                Some(0) | None => Ok(out),
                Some(code) => {
                    let said = String::from_utf8_lossy(&err).trim().to_string();
                    Err(fail(if said.is_empty() { format!("exited with {code}") } else { said }))
                }
            }
        };
        tokio::time::timeout(COMMAND_TIMEOUT, work)
            .await
            .map_err(|_| fail(format!("no answer within {} s", COMMAND_TIMEOUT.as_secs())))?
    }
}

/// The session for an SSH server connection, by id.
pub async fn session_for(state: &crate::state::AppState, server_profile_id: &str) -> Result<(Session, String)> {
    let server = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .find(|p| p.id == server_profile_id)
        .cloned()
        .ok_or_else(|| AppError::Config("the SSH server connection no longer exists".into()))?;
    if server.kind != ServiceKind::Ssh {
        return Err(AppError::Config(format!("{} is not an SSH server connection", server.name)));
    }
    let secret = crate::secrets::get_secret(&server.secret_key()).await?;
    let session = Session::open(&Target::from_profile(&server, secret)?, &known_hosts()).await?;
    Ok((session, format!("{} over SSH", server.name)))
}

/// A server's files, read with fixed commands over one session.
pub struct SshFiles {
    pub session: Session,
    pub place: String,
}

#[async_trait]
impl Reader for SshFiles {
    async fn list(&self, path: &str, include: Option<&str>) -> Result<Vec<String>> {
        let path = path.trim_end_matches('/');
        // `-type f` leaves links out, so a pattern cannot reach past its folder.
        let command = format!("LC_ALL=C find {} -maxdepth 4 -type f 2>/dev/null | head -n 5000", quote(path));
        let output = self.session.run(&command, 1024 * 1024).await?;
        let found: Vec<String> = String::from_utf8_lossy(&output).lines().map(str::to_string).collect();
        if found.len() == 1 && found[0] == path {
            return Ok(found);
        }
        crate::files::select_paths(path, include, found)
    }

    async fn stat(&self, path: &str) -> Result<FileStat> {
        // GNU and BusyBox spell it -c; a BSD or macOS server, -f.
        let path = quote(path);
        let command = format!("LC_ALL=C stat -c '%s %i %Y' -- {path} 2>/dev/null || LC_ALL=C stat -f '%z %i %m' -- {path}");
        let output = self.session.run(&command, 256).await?;
        let text = String::from_utf8_lossy(&output);
        crate::files::parse_stat(&text)
            .ok_or_else(|| AppError::Collector(format!("{path} on {}: unexpected stat output `{}`", self.place, text.trim())))
    }

    async fn read(&self, path: &str, offset: u64, max: u64) -> Result<Vec<u8>> {
        let command = format!("tail -c +{} -- {} | head -c {max}", offset + 1, quote(path));
        self.session.run(&command, max as usize).await
    }

    fn place(&self) -> Option<String> {
        Some(self.place.clone())
    }
}

/// An SSH server as a connection: reachable, as which user, and how loaded.
pub struct SshCollector {
    profile_id: String,
    profile_name: String,
    target: Target,
}

impl SshCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        Ok(SshCollector {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            target: Target::from_profile(profile, secret)?,
        })
    }
}

#[async_trait]
impl crate::collectors::Collector for SshCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Ssh
    }

    async fn collect(&self) -> Result<crate::model::HealthSnapshot> {
        let started = std::time::Instant::now();
        let session = Session::open(&self.target, &known_hosts()).await?;
        let uptime = session
            .run("uptime", 1024)
            .await
            .map(|out| String::from_utf8_lossy(&out).trim().to_string())
            .unwrap_or_default();
        Ok(crate::model::HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Ssh,
            health: crate::model::Health::Healthy,
            summary: if uptime.is_empty() { format!("Reachable as {}", self.target.user) } else { uptime },
            metrics: Vec::new(),
            sections: Vec::new(),
            checked_at: Utc::now(),
            latency_ms: crate::collectors::elapsed_ms(started),
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<crate::model::LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_path_stays_one_word_whatever_it_holds() {
        assert_eq!(quote("/var/log/app.log"), "'/var/log/app.log'");
        assert_eq!(quote("/tmp/it's; rm -rf /"), r"'/tmp/it'\''s; rm -rf /'");
    }

    #[test]
    fn a_host_key_is_pinned_once_and_a_different_one_refused() {
        let file = std::env::temp_dir().join(format!("root-known-hosts-{}.json", uuid::Uuid::new_v4()));
        assert!(admit(&file, "web-1:22", "SHA256:aaa").is_ok(), "first sight pins it");
        assert!(admit(&file, "web-1:22", "SHA256:aaa").is_ok(), "the same key is admitted");
        let refused = admit(&file, "web-1:22", "SHA256:bbb").unwrap_err();
        assert!(refused.contains("SHA256:aaa") && refused.contains("SHA256:bbb"), "{refused}");
        assert!(admit(&file, "web-2:22", "SHA256:bbb").is_ok(), "pins are per host");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn an_endpoint_names_host_port_and_user() {
        let at = |endpoint: &str, user: Option<&str>| address_of(endpoint, user).map_err(|e| e.to_string());
        assert_eq!(at("reader@web-1.internal:2222", None).unwrap(), ("web-1.internal".into(), 2222, "reader".into()));
        assert_eq!(at("web-1.internal", Some("ops")).unwrap(), ("web-1.internal".into(), 22, "ops".into()));
        assert_eq!(at("reader@web-1", Some("ops")).unwrap().2, "ops", "the username field wins");
        assert!(at("web-1.internal", None).is_err(), "a user is required");
        assert!(at("web-1:ssh", Some("ops")).is_err(), "a port is a number");
    }

    /// A real SSH server inside the test: russh's own, with keys from
    /// ssh-keygen, running each exec with `sh -c` on this machine.
    #[derive(Clone)]
    struct Exec {
        allowed: russh::keys::PublicKey,
    }

    impl russh::server::Handler for Exec {
        type Error = russh::Error;

        async fn auth_publickey(
            &mut self,
            _user: &str,
            key: &russh::keys::PublicKey,
        ) -> std::result::Result<russh::server::Auth, Self::Error> {
            Ok(if key.key_data() == self.allowed.key_data() { russh::server::Auth::Accept } else { russh::server::Auth::reject() })
        }

        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> std::result::Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: russh::ChannelId,
            data: &[u8],
            session: &mut russh::server::Session,
        ) -> std::result::Result<(), Self::Error> {
            let command = String::from_utf8_lossy(data).into_owned();
            let handle = session.handle();
            session.channel_success(channel)?;
            tokio::spawn(async move {
                let out = tokio::process::Command::new("sh").arg("-c").arg(&command).output().await.expect("sh");
                let _ = handle.data(channel, out.stdout).await;
                let _ = handle.extended_data(channel, 1, out.stderr).await;
                let _ = handle.exit_status_request(channel, out.status.code().unwrap_or(1) as u32).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
            });
            Ok(())
        }
    }

    fn keygen(dir: &Path, name: &str) -> String {
        let path = dir.join(name);
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&path)
            .status()
            .expect("ssh-keygen is on this machine");
        assert!(made.success());
        std::fs::read_to_string(&path).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_server_is_pinned_authenticated_and_read_with_fixed_commands() {
        let dir = std::env::temp_dir().join(format!("root-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("app.log"), b"0123456789\nsecond line\n").unwrap();
        std::fs::write(dir.join("conf.d/a.conf"), b"listen 80;\n").unwrap();
        std::fs::write(dir.join("conf.d/b.txt"), b"notes\n").unwrap();
        let host_key = russh::keys::decode_secret_key(&keygen(&dir, "host"), None).unwrap();
        let client_pem = keygen(&dir, "client");
        let client_public = russh::keys::decode_secret_key(&client_pem, None).unwrap().public_key().clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (config, handler) = (Arc::clone(&config), Exec { allowed: client_public.clone() });
                tokio::spawn(async move {
                    if let Ok(running) = russh::server::run_stream(config, socket, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });

        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "s", "name": "local", "kind": "ssh", "endpoint": format!("127.0.0.1:{port}"),
            "username": "tester", "enabled": true, "options": {}
        }))
        .unwrap();
        let pins = dir.join("known_hosts.json");
        let target = Target::from_profile(&profile, Some(client_pem)).unwrap();
        let files = SshFiles { session: Session::open(&target, &pins).await.unwrap(), place: "local over SSH".into() };

        let root = dir.to_string_lossy().into_owned();
        let log = format!("{root}/app.log");
        assert_eq!(files.list(&root, Some("conf.d/*.conf")).await.unwrap(), vec![format!("{root}/conf.d/a.conf")]);
        assert_eq!(files.list(&log, None).await.unwrap(), vec![log.clone()], "a file names itself");
        let stat = files.stat(&log).await.unwrap();
        assert_eq!(stat.size, 23);
        assert!(!stat.identity.is_empty(), "an inode, so a rotation is seen");
        assert_eq!(files.read(&log, 2, 5).await.unwrap(), b"23456");
        assert!(files.stat(&format!("{root}/missing.log")).await.is_err());

        let pinned = std::fs::read_to_string(&pins).unwrap();
        assert!(pinned.contains(&format!("127.0.0.1:{port}")) && pinned.contains("SHA256:"), "{pinned}");
        assert!(Session::open(&target, &pins).await.is_ok(), "the pinned key is admitted again");

        let stranger = Target::from_profile(&profile, Some(keygen(&dir, "stranger"))).unwrap();
        let refused = Session::open(&stranger, &pins).await.err().expect("an unknown key is refused").to_string();
        assert!(refused.contains("did not accept the key"), "{refused}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_key_that_is_not_a_key_is_refused_before_any_connection() {
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "s", "name": "web-1", "kind": "ssh", "endpoint": "web-1", "username": "ops", "enabled": true, "options": {}
        }))
        .unwrap();
        assert!(Target::from_profile(&profile, Some("not a key".into())).is_err());
        assert!(Target::from_profile(&profile, None).is_err());
    }
}
