//! Who is asking, and what they are allowed to ask for.
//!
//! The desktop app needs none of this. It is one operator on their own machine
//! reading a config file they own, which is the only arrangement that requires
//! no trust in anything but the OS. A server is not that, and every property
//! the desktop app got for free has to be built here.
//!
//! ## Two ways in, one session
//!
//! A **shared token** (`ROOT_TOKEN`) is the machine path: a scrape, a script,
//! the bootstrap before any account exists. It has no identity attached and is
//! treated as an administrator, because whoever holds it configured the
//! server.
//!
//! An **account** is the human path — an email, a password, a role, and an
//! audit trail of who did what. Accounts need a database; without one, the
//! shared token is the only way in and this module says so rather than
//! pretending to have users.
//!
//! OIDC will be a third way to establish the same session, which is why the
//! session is a separate concept from the password: adding a sign-in method
//! should not touch anything that reads a session.
//!
//! ## What is stored
//!
//! Passwords are Argon2id hashes. Session **tokens are not stored at all** —
//! only their SHA-256, so a database that leaks hands over no live sessions.
//! That asymmetry is the point: the token in the client's hands and the row in
//! the table are not the same value, and only one of them can be replayed.
//!
//! ## Roles
//!
//! Three, and the list of what a viewer may not do is explicit rather than
//! inferred from a command's name. `save_` and `delete_` are easy to spot;
//! `refresh_connection` reaches out to live infrastructure and `run_row_action`
//! restarts containers, and a heuristic over prefixes would call both of those
//! reads. The list is in this file where it can be reviewed.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::error::{AppError, Result};

/// How long a session lasts without being used.
///
/// Long enough that an operator is not signing in during an incident, short
/// enough that a laptop left on a train stops working before the week is out.
const SESSION_DAYS: i64 = 7;

/// Minimum password length. Deliberately a floor and not a character-class
/// rule: length is the property that matters and the rest mostly teaches
/// people to write `Password1!`.
const MIN_PASSWORD: usize = 12;

/// What someone is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Everything, including managing other people.
    Admin,
    /// Everything about the estate, nothing about accounts.
    Operator,
    /// Reads only. May not touch the estate or its configuration.
    Viewer,
}

impl Role {
    fn parse(raw: &str) -> Role {
        match raw {
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            // An unreadable role is the least privilege, never the most.
            _ => Role::Viewer,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Viewer => "viewer",
        }
    }
}

/// Commands that change something — the estate, its configuration, or this
/// server's own settings.
///
/// Written out rather than derived from a prefix. `refresh_connection` polls
/// live infrastructure and `run_row_action` restarts containers; both read as
/// reads to a naming heuristic and are not. When in doubt a command belongs on
/// this list, because the cost of a viewer being refused something harmless is
/// an error message and the cost of the reverse is an outage.
pub const MUTATING: &[&str] = &[
    "save_profile",
    "delete_profile",
    "test_profile",
    "refresh_connection",
    "run_row_action",
    "acknowledge_alert",
    "clear_logs",
    "clear_claims",
    "clear_conversation",
    "save_llm_settings",
    "save_ingest_settings",
    "rotate_ingest_token",
    "save_mcp_settings",
    "rotate_mcp_token",
    "save_prediction_settings",
    "run_prediction",
    "save_team_server",
    "sign_in",
    "sign_out",
    "send_message",
    "explain_component",
    "explain_incident",
    "resolve_confirmation",
    "record_verdict",
    "record_recall",
    "run_backtest",
    "run_scenarios",
    "test_snapshot_write",
];

/// Whether a role may run a command.
pub fn may(role: Role, command: &str) -> bool {
    match role {
        Role::Admin | Role::Operator => true,
        Role::Viewer => !MUTATING.contains(&command),
    }
}

/// Who is making this request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    /// `None` for the shared token, which has no account behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub email: String,
    pub display_name: String,
    pub role: Role,
}

impl Identity {
    /// The shared token's identity. Named so it is obvious in an audit trail
    /// that no person was attached to the request.
    pub fn machine() -> Self {
        Identity {
            user_id: None,
            email: "shared-token".to_string(),
            display_name: "Shared token".to_string(),
            role: Role::Admin,
        }
    }
}

/// An account, as returned to a client. Never carries a hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub disabled: bool,
    /// Whether this account can sign in with a password at all. False for one
    /// created by OIDC, which has no password to check.
    pub has_password: bool,
}

/// Accounts and sessions, in the same database the history lives in.
#[derive(Clone)]
pub struct Directory {
    pool: PgPool,
}

impl Directory {
    pub async fn open(pool: PgPool) -> Result<Self> {
        migrate(&pool).await?;
        Ok(Directory { pool })
    }

    /// Create an account. The first one is an administrator, because a server
    /// with no way in is a server somebody has to delete.
    pub async fn create(
        &self,
        email: &str,
        password: &str,
        display_name: &str,
        role: Option<Role>,
    ) -> Result<Account> {
        let email = normalize(email);
        if !email.contains('@') {
            return Err(AppError::Rejected("that is not an email address".into()));
        }
        if password.chars().count() < MIN_PASSWORD {
            return Err(AppError::Rejected(format!(
                "a password needs at least {MIN_PASSWORD} characters"
            )));
        }

        let first = self.count().await? == 0;
        let role = role.unwrap_or(if first { Role::Admin } else { Role::Viewer });

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, display_name, role, created_at, disabled)
             VALUES ($1,$2,$3,$4,$5,$6,FALSE)",
        )
        .bind(&id)
        .bind(&email)
        .bind(hash_password(password)?)
        .bind(display_name.trim())
        .bind(role.as_str())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("duplicate") || e.to_string().contains("unique") {
                AppError::Rejected("there is already an account with that email".into())
            } else {
                AppError::Collector(e.to_string())
            }
        })?;

        Ok(Account {
            id,
            email,
            display_name: display_name.trim().to_string(),
            role,
            created_at: now,
            disabled: false,
            has_password: true,
        })
    }

    /// Check a password and open a session. Returns the token exactly once.
    pub async fn sign_in(&self, email: &str, password: &str) -> Result<(String, Identity)> {
        let email = normalize(email);
        let row = sqlx::query(
            "SELECT id, email, password_hash, display_name, role, disabled
             FROM users WHERE email = $1",
        )
        .bind(&email)
        .fetch_optional(&self.pool)
        .await?;

        // One message for every failure. A distinct "no such account" tells an
        // attacker which addresses are worth guessing passwords for.
        let wrong = || AppError::Rejected("that email and password do not match".to_string());

        let Some(row) = row else {
            // Hash anyway. Returning early on an unknown address makes the two
            // cases measurably different in time, which is the same leak by
            // another route.
            let _ = hash_password(password);
            return Err(wrong());
        };

        if row.get::<bool, _>("disabled") {
            return Err(AppError::Rejected("that account is disabled".into()));
        }

        let stored: Option<String> = row.try_get("password_hash").ok().flatten();
        let Some(stored) = stored else {
            return Err(AppError::Rejected(
                "that account signs in with single sign-on".into(),
            ));
        };
        if !verify_password(password, &stored) {
            return Err(wrong());
        }

        let identity = Identity {
            user_id: Some(row.get("id")),
            email: row.get("email"),
            display_name: row.get("display_name"),
            role: Role::parse(&row.get::<String, _>("role")),
        };
        let token = self.open_session(identity.user_id.as_deref().unwrap()).await?;
        Ok((token, identity))
    }

    /// Start a session for an already-established identity.
    ///
    /// Separate from the password check so a second sign-in method — OIDC —
    /// reaches the same sessions without touching anything that reads one.
    pub async fn open_session(&self, user_id: &str) -> Result<String> {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen)
             VALUES ($1,$2,$3,$4,$3)",
        )
        .bind(digest(&token))
        .bind(user_id)
        .bind(now)
        .bind(now + Duration::days(SESSION_DAYS))
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    /// Resolve a session token to whoever holds it, refreshing its expiry.
    pub async fn identify(&self, token: &str) -> Option<Identity> {
        let row = sqlx::query(
            "SELECT u.id, u.email, u.display_name, u.role, u.disabled, s.expires_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1",
        )
        .bind(digest(token))
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;

        if row.get::<bool, _>("disabled") {
            return None;
        }
        if row.get::<DateTime<Utc>, _>("expires_at") < Utc::now() {
            return None;
        }

        // Sliding expiry: somebody using the tool all day should not be signed
        // out mid-incident because they signed in seven days ago.
        let _ = sqlx::query(
            "UPDATE sessions SET last_seen = $2, expires_at = $3 WHERE token_hash = $1",
        )
        .bind(digest(token))
        .bind(Utc::now())
        .bind(Utc::now() + Duration::days(SESSION_DAYS))
        .execute(&self.pool)
        .await;

        Some(Identity {
            user_id: Some(row.get("id")),
            email: row.get("email"),
            display_name: row.get("display_name"),
            role: Role::parse(&row.get::<String, _>("role")),
        })
    }

    /// End one session.
    pub async fn sign_out(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(digest(token))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// End every session for an account — what "sign out everywhere" and
    /// disabling somebody both need.
    pub async fn revoke_all(&self, user_id: &str) -> Result<u64> {
        let done = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    pub async fn list(&self) -> Result<Vec<Account>> {
        let rows = sqlx::query(
            "SELECT id, email, display_name, role, created_at, disabled,
                    password_hash IS NOT NULL AS has_password
             FROM users ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| Account {
                id: r.get("id"),
                email: r.get("email"),
                display_name: r.get("display_name"),
                role: Role::parse(&r.get::<String, _>("role")),
                created_at: r.get("created_at"),
                disabled: r.get("disabled"),
                has_password: r.try_get("has_password").unwrap_or(false),
            })
            .collect())
    }

    /// Change somebody's role or disable them. Disabling also ends every
    /// session they have, or they keep working until their token expires.
    pub async fn update(&self, user_id: &str, role: Option<Role>, disabled: Option<bool>) -> Result<()> {
        if let Some(role) = role {
            sqlx::query("UPDATE users SET role = $2 WHERE id = $1")
                .bind(user_id)
                .bind(role.as_str())
                .execute(&self.pool)
                .await?;
        }
        if let Some(disabled) = disabled {
            sqlx::query("UPDATE users SET disabled = $2 WHERE id = $1")
                .bind(user_id)
                .bind(disabled)
                .execute(&self.pool)
                .await?;
            if disabled {
                self.revoke_all(user_id).await?;
            }
        }
        Ok(())
    }

    pub async fn count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("n"))
    }

    /// Drop sessions that have expired. Cheap, and worth doing on startup so
    /// the table does not grow without bound on a long-lived server.
    pub async fn sweep(&self) -> Result<u64> {
        let done = sqlx::query("DELETE FROM sessions WHERE expires_at < $1")
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

/// SHA-256 of a session token.
///
/// The table holds this and never the token itself, so a database that leaks
/// hands over no session anybody can replay.
fn digest(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize(email: &str) -> String {
    email.trim().to_lowercase()
}

fn hash_password(password: &str) -> Result<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;

    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Collector(format!("could not hash the password: {e}")))
}

fn verify_password(password: &str, stored: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;

    PasswordHash::new(stored)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id            TEXT PRIMARY KEY,
            email         TEXT NOT NULL UNIQUE,
            password_hash TEXT,
            display_name  TEXT NOT NULL,
            role          TEXT NOT NULL,
            created_at    TIMESTAMPTZ NOT NULL,
            disabled      BOOLEAN NOT NULL DEFAULT FALSE
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token_hash TEXT PRIMARY KEY,
            user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL,
            expires_at TIMESTAMPTZ NOT NULL,
            last_seen  TIMESTAMPTZ NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS sessions_user ON sessions (user_id)")
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_viewer_may_read_and_may_not_touch_the_estate() {
        assert!(may(Role::Viewer, "get_dashboard"));
        assert!(may(Role::Viewer, "query_logs"));
        assert!(!may(Role::Viewer, "run_row_action"));
        assert!(!may(Role::Viewer, "save_profile"));
        // The two a prefix heuristic would have got wrong.
        assert!(!may(Role::Viewer, "refresh_connection"));
        assert!(!may(Role::Viewer, "test_profile"));
    }

    #[test]
    fn an_operator_may_do_anything_to_the_estate() {
        for command in MUTATING {
            assert!(may(Role::Operator, command), "{command}");
        }
    }

    #[test]
    fn every_mutating_command_is_a_real_command() {
        // A typo here would silently grant a viewer something.
        for command in MUTATING {
            assert!(
                crate::rpc::known().contains(command),
                "{command} is on the mutating list but is not a command"
            );
        }
    }

    #[test]
    fn an_unreadable_role_is_the_least_privilege() {
        assert_eq!(Role::parse("root"), Role::Viewer);
        assert_eq!(Role::parse(""), Role::Viewer);
        assert_eq!(Role::parse("admin"), Role::Admin);
    }

    #[test]
    fn a_password_verifies_against_its_own_hash_and_nothing_else() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("Correct horse battery staple", &hash));
        assert!(!verify_password("", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // A shared salt would make the table a rainbow-table lookup.
        let a = hash_password("correct horse battery staple").unwrap();
        let b = hash_password("correct horse battery staple").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_stored_hash_is_not_the_password() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(!hash.contains("correct horse"));
        assert!(hash.starts_with("$argon2id$"));
    }

    #[test]
    fn a_session_token_is_not_what_is_stored() {
        let token = "abc123";
        assert_ne!(digest(token), token);
        assert_eq!(digest(token), digest(token), "the digest must be stable");
        assert_eq!(digest(token).len(), 64);
    }

    #[test]
    fn emails_match_regardless_of_how_they_were_typed() {
        assert_eq!(normalize("  Ops@Example.COM "), "ops@example.com");
    }

    #[test]
    fn the_machine_identity_is_obviously_not_a_person() {
        let machine = Identity::machine();
        assert!(machine.user_id.is_none());
        assert_eq!(machine.role, Role::Admin);
        assert_eq!(machine.email, "shared-token");
    }
}
