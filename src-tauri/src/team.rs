//! Shared workspaces, against a root server you run yourself.
//!
//! root is local-first and stays that way. With no server configured it reads
//! a config file on this machine and answers to nobody, which is the mode that
//! needs no trust in anything beyond the OS keychain. Everything in this
//! module is additive and opt-in.
//!
//! The one thing a server adds is a shared identity: several people looking at
//! the same estate, told apart, with a role attached. That is worth being
//! precise about, because it is easy to build the *appearance* of security
//! here and nothing else.
//!
//! So: this client stores what the server told it and offers what that role
//! can do. It does not enforce anything. A role cached on this machine is a
//! hint about which buttons to draw, never a permission check — a client that
//! enforced its own permissions would be enforcing nothing, since the person
//! it is enforcing against owns the machine it runs on. Every mutating call
//! still goes to the server, which is the only place a `Viewer` can actually
//! be stopped.
//!
//! The wire protocol is deliberately small, and it is written down here
//! because it has to be implemented on the other side:
//!
//! ```text
//! POST {server}/v1/auth/sign_in   {email, password}  -> {token, account}
//! POST {server}/v1/auth/sign_out  Authorization: Bearer
//! GET  {server}/v1/members        Authorization: Bearer -> {members, sso}
//! ```

use serde::{Deserialize, Serialize};

use crate::config::{Account, Role};
use crate::error::{AppError, Result};

/// Where the session token lives. Beside the connection credentials, in the
/// OS keychain — never in the config file, which is plain text on disk.
const TOKEN_KEY: &str = "team:session";

/// One member of the workspace, as the server reports them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub email: String,
    pub name: String,
    pub role: Role,
    /// Absent for someone invited who has not signed in yet.
    pub last_seen: Option<String>,
    /// True when this member arrived through the SSO domain rather than an invite.
    #[serde(default)]
    pub via_sso: bool,
}

/// Single sign-on, as configured on the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoSettings {
    /// Anyone with an address at this domain may sign in without an invite.
    pub domain: Option<String>,
    /// What a domain match grants. Viewer unless the server says otherwise —
    /// joining a workspace should never be an escalation.
    #[serde(default = "default_sso_role")]
    pub default_role: Role,
    #[serde(default)]
    pub enforced: bool,
}

fn default_sso_role() -> Role {
    Role::Viewer
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub members: Vec<Member>,
    #[serde(default)]
    pub sso: SsoSettings,
}

#[derive(Debug, Deserialize)]
struct SignInResponse {
    token: String,
    account: Account,
}

/// Normalise a server URL so `acme.internal`, `https://acme.internal` and a
/// trailing slash all mean the same thing.
///
/// Plain `http://` is left alone rather than silently upgraded: a self-hosted
/// server on a private network may legitimately not have TLS, and quietly
/// rewriting the scheme would mean the address shown is not the address used.
pub fn normalize_server(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::Config("a server address is required".into()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    if !with_scheme.starts_with("http://") && !with_scheme.starts_with("https://") {
        return Err(AppError::Config(format!(
            "{url} is not an http or https address"
        )));
    }
    Ok(with_scheme)
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Config(format!("could not build an http client: {e}")))
}

/// Exchange credentials for a session.
///
/// The password is used here and dropped; only the token is kept, and it goes
/// to the keychain rather than the config file.
pub async fn sign_in(server_url: &str, email: &str, password: &str) -> Result<Account> {
    let server = normalize_server(server_url)?;
    let response = client()?
        .post(format!("{server}/v1/auth/sign_in"))
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(|e| unreachable_server(&server, e))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::Config(
            "that email and password were not accepted".into(),
        ));
    }
    if !response.status().is_success() {
        return Err(AppError::Config(format!(
            "{server} answered {} to the sign-in",
            response.status()
        )));
    }

    let body: SignInResponse = response
        .json()
        .await
        .map_err(|e| AppError::Config(format!("{server} sent a sign-in response root could not read: {e}")))?;

    crate::secrets::set_secret(TOKEN_KEY, &body.token).await?;
    Ok(body.account)
}

/// Drop the session locally and tell the server, in that order.
///
/// Local first on purpose: if the server is unreachable the operator still
/// stops being signed in on this machine, which is the half they asked for and
/// the half this side can guarantee.
pub async fn sign_out(server_url: Option<&str>) -> Result<()> {
    let token = crate::secrets::get_secret(TOKEN_KEY).await?;
    crate::secrets::delete_secret(TOKEN_KEY).await?;

    if let (Some(server), Some(token)) = (server_url, token) {
        let server = normalize_server(server)?;
        // Best effort: the local session is already gone.
        let _ = client()?
            .post(format!("{server}/v1/auth/sign_out"))
            .bearer_auth(token)
            .send()
            .await;
    }
    Ok(())
}

/// Who else is in this workspace.
pub async fn workspace(server_url: &str) -> Result<Workspace> {
    let server = normalize_server(server_url)?;
    let Some(token) = crate::secrets::get_secret(TOKEN_KEY).await? else {
        return Err(AppError::Config("not signed in".into()));
    };

    let response = client()?
        .get(format!("{server}/v1/members"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| unreachable_server(&server, e))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::Config("this session is no longer valid".into()));
    }
    if !response.status().is_success() {
        return Err(AppError::Config(format!(
            "{server} answered {} for the member list",
            response.status()
        )));
    }

    response
        .json()
        .await
        .map_err(|e| AppError::Config(format!("{server} sent a member list root could not read: {e}")))
}

/// Whether a session token exists on this machine at all.
pub async fn has_session() -> bool {
    crate::secrets::get_secret(TOKEN_KEY)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// The error an operator is most likely to see while there is no server yet,
/// so it says what to do rather than quoting a transport failure.
fn unreachable_server(server: &str, error: reqwest::Error) -> AppError {
    AppError::Config(format!(
        "could not reach {server}: {error}. root works without a server — clear the address in \
         Settings to go back to local mode."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_gets_https() {
        assert_eq!(normalize_server("acme.internal").unwrap(), "https://acme.internal");
    }

    /// A self-hosted server on a private network may have no TLS, and rewriting
    /// the scheme would mean the address shown is not the address used.
    #[test]
    fn plain_http_is_left_alone() {
        assert_eq!(
            normalize_server("http://10.0.0.4:8080").unwrap(),
            "http://10.0.0.4:8080"
        );
    }

    #[test]
    fn trailing_slashes_and_padding_do_not_make_a_different_server() {
        assert_eq!(
            normalize_server("  https://acme.internal/  ").unwrap(),
            "https://acme.internal"
        );
    }

    #[test]
    fn an_empty_address_is_rejected_rather_than_defaulted() {
        assert!(normalize_server("   ").is_err());
    }

    #[test]
    fn a_non_http_scheme_is_rejected() {
        assert!(normalize_server("ftp://acme.internal").is_err());
    }

    /// Viewer is the floor: an SSO domain match must never arrive with more
    /// authority than an explicit invite would have granted.
    #[test]
    fn sso_defaults_to_the_lowest_role() {
        let sso: SsoSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(sso.default_role, Role::Viewer);
        assert!(!sso.enforced);
    }

    #[test]
    fn roles_order_by_authority() {
        assert!(Role::Owner > Role::Admin);
        assert!(Role::Admin > Role::Responder);
        assert!(Role::Responder > Role::Viewer);
    }
}
