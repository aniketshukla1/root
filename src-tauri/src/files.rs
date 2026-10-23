//! Reading declared files wherever they live.
//!
//! A connection's configuration and its logs are declared as a path and a
//! place. Every place answers the same three questions — which files does this
//! path name, how big is this one and is it still the same file, and what are
//! its bytes from here on — and everything above this module asks only those.
//! A new place is one more implementation, and none of them can write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::{AppError, Result};
use crate::model::ServiceKind;

/// Most files one declared path may name, so a pattern over a big tree is bounded.
pub const MAX_FILES: usize = 200;

/// A file inside a container is fetched whole, so it has to be small.
pub const CONTAINER_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// A folder inside a container is fetched whole to list it.
const CONTAINER_FOLDER_BYTES: usize = 16 * 1024 * 1024;

/// Where a declared path is read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Location {
    /// The machine root runs on.
    #[default]
    Local,
    /// Inside a container on a Docker or Podman connection, through the
    /// engine's archive API: a stat and a tar of the path. No exec, no shell,
    /// nothing run inside the container.
    Container { host_profile_id: String, container: String },
    /// On a server, over an SSH server connection: fixed read-only commands
    /// with every path quoted. See [`crate::ssh`].
    Ssh { server_profile_id: String },
}

impl Location {
    /// Whether a path is absolute where it will be read.
    pub fn is_absolute(&self, path: &str) -> bool {
        match self {
            Location::Local => Path::new(path).is_absolute(),
            Location::Container { .. } | Location::Ssh { .. } => path.starts_with('/'),
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Location::Local => Ok(()),
            Location::Container { host_profile_id, container } => {
                if host_profile_id.trim().is_empty() {
                    return Err(AppError::Config("choose the Docker or Podman connection the container runs on".into()));
                }
                let name = container.trim();
                let fits = !name.is_empty()
                    && name.len() <= 128
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
                if !fits {
                    return Err(AppError::Config(format!("`{name}` is not a container name or id")));
                }
                Ok(())
            }
            Location::Ssh { server_profile_id } if server_profile_id.trim().is_empty() => {
                Err(AppError::Config("choose the SSH server connection to read through".into()))
            }
            Location::Ssh { .. } => Ok(()),
        }
    }
}

/// One file, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    /// Changes when the path starts naming a different file, as a rotated log
    /// does: the inode, where there is one. Empty where the place has none to
    /// offer; a rotation is then seen only when the file shrinks.
    pub identity: String,
    pub modified: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait Reader: Send + Sync {
    /// The files `path` names: itself when it is a file, else the files under
    /// it that `include` matches (every file directly inside when absent).
    /// Sorted, at most [`MAX_FILES`], never outside `path`.
    async fn list(&self, path: &str, include: Option<&str>) -> Result<Vec<String>>;
    async fn stat(&self, path: &str) -> Result<FileStat>;
    /// At most `max` bytes of `path`, from `offset`.
    async fn read(&self, path: &str, offset: u64, max: u64) -> Result<Vec<u8>>;
    /// Where this reads, for anything shown: absent for this machine.
    fn place(&self) -> Option<String> {
        None
    }
}

/// The reader for a place. Places that need a connection resolve it here.
pub async fn reader(state: &crate::state::AppState, location: &Location) -> Result<Box<dyn Reader>> {
    match location {
        Location::Local => Ok(Box::new(Local)),
        Location::Container { host_profile_id, container } => {
            let host = state
                .config
                .read()
                .await
                .profiles
                .iter()
                .find(|p| &p.id == host_profile_id)
                .cloned()
                .ok_or_else(|| AppError::Config("the container's Docker connection no longer exists".into()))?;
            if !matches!(host.kind, ServiceKind::Docker | ServiceKind::Podman) {
                return Err(AppError::Config(format!("{} is not a Docker or Podman connection", host.name)));
            }
            Ok(Box::new(ContainerFiles {
                docker: crate::collectors::docker::connect(&host)?,
                container: container.trim().to_string(),
                place: format!("container {} on {}", container.trim(), host.name),
                fetched: std::sync::Mutex::new(HashMap::new()),
            }))
        }
        Location::Ssh { server_profile_id } => {
            let (session, place) = crate::ssh::session_for(state, server_profile_id).await?;
            Ok(Box::new(crate::ssh::SshFiles { session, place }))
        }
    }
}

/// This machine's own filesystem.
pub struct Local;

fn io(path: &str, e: std::io::Error) -> AppError {
    AppError::Collector(format!("{path}: {e}"))
}

#[async_trait]
impl Reader for Local {
    async fn list(&self, path: &str, include: Option<&str>) -> Result<Vec<String>> {
        let root = PathBuf::from(path);
        let include = include.map(str::to_string);
        let found = tokio::task::spawn_blocking(move || expand(&root, include.as_deref()))
            .await
            .map_err(|e| AppError::Collector(format!("listing {e}")))??;
        Ok(found.into_iter().map(|p| p.to_string_lossy().into_owned()).collect())
    }

    async fn stat(&self, path: &str) -> Result<FileStat> {
        let metadata = tokio::fs::metadata(path).await.map_err(|e| io(path, e))?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = metadata
            .created()
            .map(|t| format!("{:?}", t))
            .unwrap_or_default();
        Ok(FileStat {
            size: metadata.len(),
            identity,
            modified: metadata.modified().ok().map(DateTime::<Utc>::from),
        })
    }

    async fn read(&self, path: &str, offset: u64, max: u64) -> Result<Vec<u8>> {
        let mut file = tokio::fs::File::open(path).await.map_err(|e| io(path, e))?;
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| io(path, e))?;
        let mut out = Vec::new();
        file.take(max).read_to_end(&mut out).await.map_err(|e| io(path, e))?;
        Ok(out)
    }
}

/// Every file a local path names, bounded and rooted.
pub fn expand(root: &Path, include: Option<&str>) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(AppError::Collector("no such file or directory".to_string()));
    }

    let pattern = include.unwrap_or("*");
    if pattern.split(['/', '\\']).any(|c| c == "..") {
        return Err(AppError::Collector(
            "the include pattern may not contain `..` — a source reaches into its own \
             directory, not out of it"
                .to_string(),
        ));
    }

    let joined = root.join(pattern);
    let glob = joined.to_string_lossy();
    let matches = glob::glob(&glob).map_err(|e| AppError::Collector(format!("bad include pattern: {e}")))?;

    // Resolve every hit against the real root, so a symlink inside the
    // directory cannot quietly widen the source to somewhere else on the
    // machine. A source is a statement about one directory.
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut out = Vec::new();
    for entry in matches.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if !entry.is_file() {
            continue;
        }
        let real = match entry.canonicalize() {
            Ok(real) => real,
            Err(_) => continue,
        };
        if !real.starts_with(&real_root) {
            continue;
        }
        out.push(entry);
    }

    out.sort();
    Ok(out)
}

/// The engine reports a Go `os.FileMode`; these are its type bits.
const MODE_DIR: u32 = 1 << 31;
const MODE_SYMLINK: u32 = 1 << 27;

/// Files inside one container, read through the engine's archive API.
pub struct ContainerFiles {
    docker: bollard::Docker,
    container: String,
    place: String,
    /// Files fetched during this reader's life (one poll), so a listing and
    /// the reads after it are one trip.
    fetched: std::sync::Mutex<HashMap<String, Vec<u8>>>,
}

impl ContainerFiles {
    fn fail(&self, path: &str, e: impl std::fmt::Display) -> AppError {
        AppError::Collector(format!("{path} in {}: {e}", self.place))
    }

    async fn archive(&self, path: &str, cap: usize) -> Result<Vec<u8>> {
        let options = bollard::query_parameters::DownloadFromContainerOptionsBuilder::default().path(path).build();
        let mut stream = self.docker.download_from_container(&self.container, Some(options));
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.map_err(|e| self.fail(path, e))?);
            if out.len() > cap {
                return Err(self.fail(path, format!("over {} MB, and a container's files are fetched whole", cap / (1024 * 1024))));
            }
        }
        Ok(out)
    }
}

/// The regular files in an archive of `root`, by their path in the container.
/// Links are left out, so a link inside a folder cannot reach outside it.
fn unpack(root: &str, archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let parent = root.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("");
    let bad = |e: std::io::Error| AppError::Collector(format!("unreadable archive of {root}: {e}"));
    let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
    let mut out = Vec::new();
    for entry in tar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let name = entry.path().map_err(bad)?.to_string_lossy().into_owned();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).map_err(bad)?;
        out.push((format!("{parent}/{name}"), body));
    }
    Ok(out)
}

/// Whether a path under `root` is one `include` names, matching as a local
/// listing does: every file directly inside when there is no pattern.
fn matcher(root: &str, include: Option<&str>) -> Result<impl Fn(&str) -> bool> {
    let pattern = glob::Pattern::new(include.unwrap_or("*"))
        .map_err(|e| AppError::Collector(format!("bad include pattern: {e}")))?;
    let options = glob::MatchOptions { require_literal_separator: true, ..Default::default() };
    let prefix = format!("{root}/");
    Ok(move |path: &str| path.strip_prefix(&prefix).is_some_and(|rel| pattern.matches_with(rel, options)))
}

fn select(root: &str, include: Option<&str>, files: Vec<(String, Vec<u8>)>) -> Result<Vec<(String, Vec<u8>)>> {
    let wanted = matcher(root, include)?;
    let mut out: Vec<(String, Vec<u8>)> = files.into_iter().filter(|(path, _)| wanted(path)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.truncate(MAX_FILES);
    Ok(out)
}

/// [`select`] for a listing of paths alone.
pub(crate) fn select_paths(root: &str, include: Option<&str>, paths: Vec<String>) -> Result<Vec<String>> {
    let wanted = matcher(root, include)?;
    let mut out: Vec<String> = paths.into_iter().filter(|path| wanted(path)).collect();
    out.sort();
    out.truncate(MAX_FILES);
    Ok(out)
}

#[async_trait]
impl Reader for ContainerFiles {
    async fn list(&self, path: &str, include: Option<&str>) -> Result<Vec<String>> {
        let path = path.trim_end_matches('/');
        let stat = self
            .docker
            .get_container_archive_info(
                &self.container,
                Some(bollard::query_parameters::ContainerArchiveInfoOptionsBuilder::default().path(path).build()),
            )
            .await
            .map_err(|e| self.fail(path, e))?;
        if stat.file_mode & MODE_DIR == 0 {
            return Ok(vec![path.to_string()]);
        }
        let archive = self.archive(path, CONTAINER_FOLDER_BYTES).await?;
        let chosen = select(path, include, unpack(path, &archive)?)?;
        let mut fetched = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        Ok(chosen
            .into_iter()
            .map(|(file, body)| {
                fetched.insert(file.clone(), body);
                file
            })
            .collect())
    }

    async fn stat(&self, path: &str) -> Result<FileStat> {
        let stat = self
            .docker
            .get_container_archive_info(
                &self.container,
                Some(bollard::query_parameters::ContainerArchiveInfoOptionsBuilder::default().path(path).build()),
            )
            .await
            .map_err(|e| self.fail(path, e))?;
        if stat.file_mode & MODE_SYMLINK != 0 {
            // The official nginx image links its logs to the container's
            // output, which root already reads as the container's own logs.
            return Err(self.fail(path, format!("it is a link to {}, not a file", stat.link_target)));
        }
        if stat.file_mode & MODE_DIR != 0 {
            return Err(self.fail(path, "it is a folder"));
        }
        Ok(FileStat {
            size: stat.size.max(0) as u64,
            // The archive API has no inode to offer.
            identity: String::new(),
            modified: stat
                .modification_time
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc)),
        })
    }

    async fn read(&self, path: &str, offset: u64, max: u64) -> Result<Vec<u8>> {
        let cached = self.fetched.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned();
        let body = match cached {
            Some(body) => body,
            None => {
                let size = self.stat(path).await?.size;
                if size > CONTAINER_FILE_BYTES {
                    return Err(self.fail(
                        path,
                        format!(
                            "{} MB, over the {} MB a container's file can be fetched whole; send it to the container's output, or mount it on the host",
                            size / (1024 * 1024),
                            CONTAINER_FILE_BYTES / (1024 * 1024)
                        ),
                    ));
                }
                let archive = self.archive(path, CONTAINER_FILE_BYTES as usize + 64 * 1024).await?;
                let body = unpack(path, &archive)?
                    .into_iter()
                    .find(|(file, _)| file == path)
                    .map(|(_, body)| body)
                    .ok_or_else(|| self.fail(path, "not in the archive the engine returned"))?;
                self.fetched.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), body.clone());
                body
            }
        };
        let start = (offset as usize).min(body.len());
        let end = start.saturating_add(max as usize).min(body.len());
        Ok(body[start..end].to_vec())
    }

    fn place(&self) -> Option<String> {
        Some(self.place.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_local_read_starts_where_it_is_told_and_stops_at_its_limit() {
        let dir = std::env::temp_dir().join(format!("root-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.log");
        std::fs::write(&file, b"0123456789").unwrap();
        let path = file.to_string_lossy().into_owned();

        assert_eq!(Local.read(&path, 3, 4).await.unwrap(), b"3456");
        assert_eq!(Local.read(&path, 8, 100).await.unwrap(), b"89");
        assert_eq!(Local.stat(&path).await.unwrap().size, 10);
        assert_eq!(Local.list(&dir.to_string_lossy(), Some("*.log")).await.unwrap(), vec![path]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An archive shaped the way the engine returns one for `/etc/nginx`.
    fn nginx_archive() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut add = |name: &str, body: &[u8], kind: tar::EntryType| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            if kind == tar::EntryType::Symlink {
                header.set_link_name("/etc/shadow").unwrap();
            }
            header.set_cksum();
            builder.append_data(&mut header, name, body).unwrap();
        };
        add("nginx/", b"", tar::EntryType::Directory);
        add("nginx/nginx.conf", b"worker_processes 4;\n", tar::EntryType::Regular);
        add("nginx/conf.d/default.conf", b"listen 80;\n", tar::EntryType::Regular);
        add("nginx/escape.conf", b"", tar::EntryType::Symlink);
        builder.into_inner().unwrap()
    }

    #[test]
    fn a_containers_folder_unpacks_to_its_regular_files_and_never_its_links() {
        let files = unpack("/etc/nginx", &nginx_archive()).unwrap();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["/etc/nginx/nginx.conf", "/etc/nginx/conf.d/default.conf"]);
        assert_eq!(files[0].1, b"worker_processes 4;\n");
    }

    #[test]
    fn a_pattern_in_a_container_matches_as_it_does_on_this_machine() {
        let files = || unpack("/etc/nginx", &nginx_archive()).unwrap();
        let direct: Vec<String> = select("/etc/nginx", None, files()).unwrap().into_iter().map(|(p, _)| p).collect();
        assert_eq!(direct, vec!["/etc/nginx/nginx.conf"], "no pattern: what is directly inside");
        let deep: Vec<String> = select("/etc/nginx", Some("**/*.conf"), files()).unwrap().into_iter().map(|(p, _)| p).collect();
        assert_eq!(deep, vec!["/etc/nginx/conf.d/default.conf", "/etc/nginx/nginx.conf"]);
    }

    #[test]
    fn a_container_location_names_a_connection_and_a_real_container() {
        let at = |host: &str, container: &str| Location::Container { host_profile_id: host.into(), container: container.into() };
        assert!(at("docker-1", "web-1").validate().is_ok());
        assert!(at("", "web-1").validate().is_err());
        assert!(at("docker-1", "web; rm -rf /").validate().is_err());
        assert!(at("docker-1", "/etc/nginx").is_absolute("/etc/nginx"));
        assert!(!at("docker-1", "web").is_absolute("etc/nginx"));
    }
}
