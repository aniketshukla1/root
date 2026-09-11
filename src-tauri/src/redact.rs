//! Removing secrets from text before it can leave the process.
//!
//! Reading config is what turns a monitor into something that can say why an
//! estate is misconfigured. It is also the single most dangerous thing this
//! app does, because config files are where credentials live: `requirepass` in
//! redis.conf, the environment block of a compose file, a datasource password
//! in application.yml, a private key inlined into a TLS stanza.
//!
//! The assistant sends what it reads to a third-party API. So the rule is not
//! "redact in the prompt" — a prompt is one caller among many and the next one
//! will forget. Redaction happens in the reader, between the file and every
//! consumer, and there is no accessor that returns an unredacted body.
//!
//! ## What survives, and why that matters
//!
//! Over-redaction has a real cost: a config review that cannot see
//! `maxmemory`, `max_connections` or `worker_processes` is worthless. So this
//! aims at values that are secrets rather than at settings that merely sound
//! sensitive — `auth_method = scram-sha-256` is exactly the line an operator
//! wants reviewed, and it is not a credential.
//!
//! Two independent passes, because either alone has a gap:
//!
//! **By key.** `password = hunter2` is a secret whatever the value looks like.
//!
//! **By shape.** A private key, a JWT, `postgres://user:pw@host`, an AWS key
//! id or a provider token is a secret whatever the key is called — including
//! under a key named nothing at all, in a bare list, or in a comment.
//!
//! ## The asymmetry
//!
//! A false positive costs the model some context. A false negative puts a
//! production credential in someone else's logs, permanently. Those are not
//! comparable, so ambiguity resolves toward redaction — and every redaction is
//! reported, with its line and its reason, so an operator can see when it has
//! been too eager rather than wondering what happened to a value.
//!
//! ## What redaction is not
//!
//! It is not a reason to be relaxed about what gets read. The deny list in
//! `secretish_filename` refuses whole files whose every byte is a credential,
//! because "we redacted the private key" is not a sentence worth having to
//! say. And the deterministic rules in the advisor run inside this process on
//! the original text: the checks see everything, the model sees none of it.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// Why a value was removed. Carried to the operator rather than summarised,
/// because "3 values redacted" invites a shrug and "line 42, requirepass"
/// invites a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// The key is one that holds credentials.
    KeyName,
    /// A PEM block — private key, or anything else armoured the same way.
    PrivateKey,
    /// A JSON Web Token.
    Jwt,
    /// Credentials embedded in a URL: `scheme://user:password@host`.
    UrlCredentials,
    /// A recognisable provider token — AWS, GitHub, Slack, OpenAI, Anthropic.
    ProviderToken,
    /// An `Authorization:` header value.
    AuthorizationHeader,
}

impl Reason {
    /// What goes in the placeholder, so the shape of the file survives.
    fn label(self) -> &'static str {
        match self {
            Reason::KeyName => "redacted",
            Reason::PrivateKey => "redacted private key",
            Reason::Jwt => "redacted jwt",
            Reason::UrlCredentials => "redacted credentials",
            Reason::ProviderToken => "redacted token",
            Reason::AuthorizationHeader => "redacted authorization",
        }
    }

    fn placeholder(self) -> String {
        format!("«{}»", self.label())
    }
}

/// One value removed from one place.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redaction {
    /// 1-indexed, to match what an editor shows.
    pub line: usize,
    /// The key whose value went, when there was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub reason: Reason,
}

/// Text with its secrets removed, and the record of what was removed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redacted {
    pub text: String,
    pub redactions: Vec<Redaction>,
}

impl Redacted {
    pub fn is_clean(&self) -> bool {
        self.redactions.is_empty()
    }
}

/// Key fragments that mean the value is a credential.
///
/// Matched as a case-insensitive substring against the key, so `db_password`,
/// `REDIS_PASSWORD` and `spring.datasource.password` all hit on `password`.
///
/// Chosen to be unambiguous. `auth` is deliberately absent: it would take
/// `auth_method`, `authentication_timeout` and pg_hba's whole vocabulary with
/// it, and those lines are the point of reviewing a config. `cert` is absent
/// for the same reason — a certificate is public, and which one is in use is
/// worth seeing. The keys that hold the matching *private* halves are here.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "access_key",
    "accesskey",
    "secretkey",
    "private_key",
    "privatekey",
    "credential",
    "authorization",
    "auth_token",
    "authtoken",
    "requirepass",
    "masterauth",
    "client_secret",
    "encryption_key",
    "signing_key",
    "session_key",
    "webhook_url",
];

/// Keys that contain a secret fragment but are not secrets.
///
/// Without these, reviewing an auth config means reviewing a wall of
/// placeholders: whether a password file is configured, how long a token
/// lives, and which algorithm signs it are all settings an operator needs to
/// see, and none of them is a credential.
const NOT_SECRET_KEYS: &[&str] = &[
    "password_file",
    "passwordfile",
    "password_path",
    "secret_file",
    "secretfile",
    "secret_path",
    "token_file",
    "tokenfile",
    "token_path",
    "credential_file",
    "credentialsfile",
    "credentials_file",
    "private_key_file",
    "private_key_path",
    "privatekeyfile",
    "password_encryption",
    "password_min_length",
    "token_ttl",
    "token_lifetime",
    "token_expiry",
    "secret_key_ref",
    "apikey_header",
    "api_key_header",
];

static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    // Covers the shapes config actually comes in: ini and conf (`key = v`,
    // `key v`), yaml (`key: v`), env (`KEY=v`), json (`"key": "v"`).
    Regex::new(r#"(?m)^(?P<lead>\s*-?\s*"?)(?P<key>[A-Za-z_][\w.\-/]*)"?\s*(?P<sep>[:=]\s*|\s+)(?P<value>\S.*?)(?P<trail>\s*,?\s*)$"#)
        .expect("key/value pattern")
});

static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("pem pattern")
});

static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\b")
        .expect("jwt pattern")
});

static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    // The password half only. The scheme, user, host, port and path are the
    // parts a reviewer needs, and blanking the whole URL would hide which
    // database a service is even pointed at.
    Regex::new(r"(?P<head>\b[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+:)(?P<secret>[^\s@/]+)(?P<tail>@)")
        .expect("url credentials pattern")
});

static PROVIDER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{20,}|gho_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_\-]{16,}|xox[baprs]-[A-Za-z0-9\-]{10,}|sk-ant-[A-Za-z0-9_\-]{20,}|sk-[A-Za-z0-9]{32,}|AIza[A-Za-z0-9_\-]{30,}|hvs\.[A-Za-z0-9_\-]{20,})\b",
    )
    .expect("provider token pattern")
});

static AUTHORIZATION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?P<head>\b(?:bearer|basic)\s+)(?P<secret>[A-Za-z0-9+/=_\-.]{12,})")
        .expect("authorization pattern")
});

/// Whether a key's value is a credential.
pub fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if NOT_SECRET_KEYS.iter().any(|k| lower.contains(k)) {
        return false;
    }
    SECRET_KEYS.iter().any(|k| lower.contains(k))
}

/// Whether a file is one whose every byte is a credential.
///
/// These are refused rather than redacted. A `.env` that came back as forty
/// lines of placeholder would be useless, and "we redacted the private key"
/// is not a sentence worth having to say — not reading it is stronger than
/// reading it carefully.
pub fn secretish_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);

    const EXACT: &[&str] = &[
        ".env",
        ".pgpass",
        ".netrc",
        "_netrc",
        ".htpasswd",
        "credentials",
        "authorized_keys",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "shadow",
        "keyring",
    ];
    const SUFFIX: &[&str] = &[
        ".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".truststore", ".kdbx", ".ppk",
        ".asc", ".gpg", ".age",
    ];

    if EXACT.contains(&base) {
        return true;
    }
    if base.starts_with(".env.") || base.starts_with("id_rsa") || base.starts_with("id_ed25519") {
        return true;
    }
    SUFFIX.iter().any(|s| base.ends_with(s))
}

/// Strip every secret this knows how to find.
///
/// Shape passes run before the key pass so that a secret under an innocuous
/// key is caught by its own form, and the key pass then cannot re-redact a
/// placeholder — `«redacted jwt»` is not a token.
pub fn text(input: &str) -> Redacted {
    let mut found: Vec<Redaction> = Vec::new();

    let staged = redact_blocks(input, &mut found);
    let staged = redact_shapes(&staged, &mut found);
    let out = redact_by_key(&staged, &mut found);

    found.sort_by_key(|r| r.line);
    Redacted {
        text: out,
        redactions: found,
    }
}

/// PEM blocks, which span lines and so cannot be handled line by line.
fn redact_blocks(input: &str, found: &mut Vec<Redaction>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    for m in PEM_BLOCK.find_iter(input) {
        out.push_str(&input[last..m.start()]);
        out.push_str(&Reason::PrivateKey.placeholder());
        found.push(Redaction {
            line: input[..m.start()].lines().count().max(1),
            key: None,
            reason: Reason::PrivateKey,
        });
        last = m.end();
    }
    out.push_str(&input[last..]);
    out
}

/// Values recognisable by their own form, wherever they appear — including in
/// comments, in bare list items, and under keys with blameless names.
fn redact_shapes(input: &str, found: &mut Vec<Redaction>) -> String {
    input
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let mut line = line.to_string();
            let note = |reason: Reason, found: &mut Vec<Redaction>| {
                found.push(Redaction {
                    line: i + 1,
                    key: None,
                    reason,
                });
            };

            if JWT.is_match(&line) {
                note(Reason::Jwt, found);
                line = JWT
                    .replace_all(&line, Reason::Jwt.placeholder())
                    .into_owned();
            }
            if PROVIDER_TOKEN.is_match(&line) {
                note(Reason::ProviderToken, found);
                line = PROVIDER_TOKEN
                    .replace_all(&line, Reason::ProviderToken.placeholder())
                    .into_owned();
            }
            if URL_CREDENTIALS.is_match(&line) {
                note(Reason::UrlCredentials, found);
                line = URL_CREDENTIALS
                    .replace_all(
                        &line,
                        format!("${{head}}{}${{tail}}", Reason::UrlCredentials.placeholder()),
                    )
                    .into_owned();
            }
            if AUTHORIZATION_HEADER.is_match(&line) {
                note(Reason::AuthorizationHeader, found);
                line = AUTHORIZATION_HEADER
                    .replace_all(
                        &line,
                        format!(
                            "${{head}}{}",
                            Reason::AuthorizationHeader.placeholder()
                        ),
                    )
                    .into_owned();
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Values whose key says what they are.
fn redact_by_key(input: &str, found: &mut Vec<Redaction>) -> String {
    input
        .lines()
        .enumerate()
        .map(|(i, line)| {
            // A comment's key/value is not a setting, and redacting inside one
            // hides the documentation that explains the setting above it. The
            // shape pass has already been over this line, so a real secret
            // parked in a comment was caught by its form.
            if line.trim_start().starts_with('#') || line.trim_start().starts_with("//") {
                return line.to_string();
            }

            let Some(caps) = KEY_VALUE.captures(line) else {
                return line.to_string();
            };
            let key = &caps["key"];
            if !is_secret_key(key) {
                return line.to_string();
            }

            // Already gone by shape, or never set. Reporting it again would
            // double-count, and blanking an empty value invents a secret.
            let value = caps["value"].trim();
            if value.is_empty() || value.starts_with('«') || is_placeholder(value) {
                return line.to_string();
            }

            found.push(Redaction {
                line: i + 1,
                key: Some(key.to_string()),
                reason: Reason::KeyName,
            });
            format!(
                "{}{}{}{}{}",
                &caps["lead"],
                key,
                &caps["sep"],
                Reason::KeyName.placeholder(),
                &caps["trail"]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Values that are already stand-ins rather than credentials.
///
/// A compose file full of `${DB_PASSWORD}` is telling you where the secret
/// comes from, not what it is, and replacing that with a placeholder loses the
/// one useful fact — that this deployment sources it from the environment.
fn is_placeholder(value: &str) -> bool {
    let v = value.trim().trim_matches(['"', '\'']);
    if v.is_empty() {
        return true;
    }
    // ${VAR}, $VAR, {{ .Values.x }}, <set-me>, ***, null
    v.starts_with("${")
        || v.starts_with("$(")
        || (v.starts_with('$') && v[1..].chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        || v.starts_with("{{")
        || v.starts_with('<')
        || v.chars().all(|c| c == '*')
        || v.eq_ignore_ascii_case("null")
        || v.eq_ignore_ascii_case("none")
}

/// The same treatment for structured data — a `docker inspect`, a pod spec, a
/// `CONFIG GET` result.
///
/// Walks the tree rather than the serialised form so that a key's name governs
/// its own value and nothing else: flattening to text first would let a long
/// value containing `password:` redact the wrong field.
pub fn json(value: &serde_json::Value) -> (serde_json::Value, Vec<Redaction>) {
    let mut found = Vec::new();
    let out = walk(value, None, &mut found);
    (out, found)
}

fn walk(
    value: &serde_json::Value,
    key: Option<&str>,
    found: &mut Vec<Redaction>,
) -> serde_json::Value {
    use serde_json::Value;

    match value {
        Value::String(s) => {
            if key.is_some_and(is_secret_key) && !is_placeholder(s) && !s.is_empty() {
                found.push(Redaction {
                    line: 0,
                    key: key.map(str::to_string),
                    reason: Reason::KeyName,
                });
                return Value::String(Reason::KeyName.placeholder());
            }
            // Shape rules still apply: a token under `value` or `args[3]` is
            // a token.
            let redacted = text(s);
            if redacted.is_clean() {
                value.clone()
            } else {
                for mut r in redacted.redactions {
                    r.key = key.map(str::to_string);
                    r.line = 0;
                    found.push(r);
                }
                Value::String(redacted.text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                // An array's items inherit the array's key, so
                // `"env": ["SECRET=x"]` and `"command": [...]` are each judged
                // by the name that introduced them.
                .map(|v| walk(v, key, found))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), walk(v, Some(k), found)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reasons(r: &Redacted) -> Vec<Reason> {
        r.redactions.iter().map(|x| x.reason).collect()
    }

    // ------------------------------------------------------------- by key

    #[test]
    fn a_password_goes_whatever_the_syntax() {
        for line in [
            "password = hunter2",
            "password: hunter2",
            "PASSWORD=hunter2",
            "requirepass hunter2",
            "  spring.datasource.password: hunter2",
            "\"client_secret\": \"hunter2\",",
        ] {
            let out = text(line);
            assert!(!out.text.contains("hunter2"), "leaked: {line} -> {}", out.text);
            assert_eq!(out.redactions.len(), 1, "{line}");
        }
    }

    /// The whole point of reading config is the settings. A reviewer who
    /// cannot see these has been handed nothing.
    #[test]
    fn settings_that_merely_sound_sensitive_survive() {
        let conf = "\
maxmemory 2gb
maxmemory-policy noeviction
max_connections = 200
auth_method = scram-sha-256
password_encryption = scram-sha-256
ssl_cert_file = '/etc/ssl/server.crt'
password_file = /run/secrets/db_password
token_ttl = 3600
worker_processes auto";
        let out = text(conf);
        assert!(out.is_clean(), "over-redacted: {:?}", out.redactions);
        assert_eq!(out.text, conf);
    }

    /// A compose file that sources its password from the environment is
    /// documenting where the secret lives. That is the useful half.
    #[test]
    fn an_environment_reference_is_not_a_secret() {
        let out = text("      POSTGRES_PASSWORD: ${DB_PASSWORD}");
        assert!(out.is_clean(), "{:?}", out.redactions);
        assert!(out.text.contains("${DB_PASSWORD}"));
    }

    // ----------------------------------------------------------- by shape

    #[test]
    fn a_private_key_goes_even_spanning_lines() {
        let pem = "ssl_certificate_key\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA1\nabc+/def==\n-----END RSA PRIVATE KEY-----\nlisten 443;";
        let out = text(pem);
        assert!(!out.text.contains("MIIEowIBAAKCAQEA1"));
        assert!(out.text.contains("listen 443;"), "kept the rest of the file");
        assert_eq!(reasons(&out), vec![Reason::PrivateKey]);
    }

    /// The case the key pass cannot reach: a real secret under a name that
    /// gives nothing away.
    #[test]
    fn a_token_under_an_innocent_key_is_still_caught() {
        let out = text("value = ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(!out.text.contains("ghp_abcdef"));
        assert_eq!(reasons(&out), vec![Reason::ProviderToken]);
    }

    #[test]
    fn a_jwt_goes_wherever_it_appears() {
        let out = text("# example: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert!(!out.text.contains("dBjftJeZ"));
        assert_eq!(reasons(&out), vec![Reason::Jwt]);
    }

    /// Only the password half. Which host a service points at is exactly what
    /// a config review is for.
    #[test]
    fn a_connection_url_keeps_everything_but_the_password() {
        let out = text("DATABASE_URL=postgres://orders:s3cr3t@db-01.internal:5432/orders");
        assert!(!out.text.contains("s3cr3t"));
        assert!(out.text.contains("postgres://orders:"));
        assert!(out.text.contains("@db-01.internal:5432/orders"));
    }

    #[test]
    fn an_authorization_header_goes() {
        let out = text("proxy_set_header Authorization \"Bearer abcdef0123456789abcdef\";");
        assert!(!out.text.contains("abcdef0123456789abcdef"));
    }

    /// Shape runs first so the key pass cannot report the same value twice.
    #[test]
    fn a_secret_caught_by_shape_is_not_counted_again_by_key() {
        let out = text("api_key = ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(out.redactions.len(), 1, "{:?}", out.redactions);
    }

    // ----------------------------------------------------------- reporting

    #[test]
    fn every_redaction_names_its_line() {
        let out = text("host = db-01\npassword = hunter2\nport = 5432");
        assert_eq!(out.redactions.len(), 1);
        assert_eq!(out.redactions[0].line, 2);
        assert_eq!(out.redactions[0].key.as_deref(), Some("password"));
    }

    #[test]
    fn structure_is_preserved_so_the_file_still_reads_as_config() {
        let out = text("  password: hunter2");
        assert_eq!(out.text, "  password: «redacted»");
    }

    // ------------------------------------------------------------ filenames

    #[test]
    fn files_that_are_entirely_secret_are_named_as_such() {
        for name in [
            ".env",
            ".env.production",
            "/srv/app/.env",
            "server.pem",
            "tls.key",
            "keystore.jks",
            "/home/deploy/.ssh/id_rsa",
            ".pgpass",
        ] {
            assert!(secretish_filename(name), "{name}");
        }
    }

    #[test]
    fn ordinary_config_is_not_on_the_deny_list() {
        for name in [
            "nginx.conf",
            "redis.conf",
            "docker-compose.yml",
            "application.yaml",
            "server.crt",
            "values.yaml",
            "postgresql.conf",
            "pg_hba.conf",
        ] {
            assert!(!secretish_filename(name), "{name}");
        }
    }

    // ---------------------------------------------------------------- json

    #[test]
    fn json_redacts_by_key_without_flattening() {
        let value = serde_json::json!({
            "Env": ["PATH=/usr/bin", "DB_PASSWORD=hunter2"],
            "Config": { "password": "hunter2", "maxmemory": "2gb" },
        });
        let (out, found) = json(&value);
        let rendered = out.to_string();
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("2gb"), "settings survive: {rendered}");
        assert!(rendered.contains("PATH=/usr/bin"));
        assert!(!found.is_empty());
    }

    /// The array case that matters: `Env` is a list of `KEY=value` strings, so
    /// the secret is inside the item rather than under a key of its own.
    #[test]
    fn an_env_list_entry_is_redacted_by_its_own_name() {
        let value = serde_json::json!({ "Env": ["DB_PASSWORD=hunter2"] });
        let (out, _) = json(&value);
        assert!(!out.to_string().contains("hunter2"), "{out}");
    }

    #[test]
    fn json_leaves_numbers_and_booleans_alone() {
        let value = serde_json::json!({ "port": 5432, "ssl": true, "name": "orders" });
        let (out, found) = json(&value);
        assert_eq!(out, value);
        assert!(found.is_empty());
    }
}
