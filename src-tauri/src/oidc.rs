//! Signing in with somebody else's identity provider.
//!
//! The third way into a root server, after the shared token and a password.
//! It establishes exactly the same session as the other two — `auth.rs` owns
//! sessions and knows nothing about how one was earned — which is why adding
//! this touched no code that reads a session.
//!
//! ## Authorization code, with PKCE
//!
//! The only flow worth implementing. The implicit flow puts tokens in a URL
//! and therefore in browser history and server logs; PKCE closes the
//! interception window in the code flow and costs one hash.
//!
//! ## Claims come from `userinfo`, not from the `id_token`
//!
//! The obvious implementation parses the `id_token` JWT, which means fetching
//! a JWKS, matching a key id, and verifying a signature — a few hundred lines
//! in which a mistake is silent and total, because a JWT parser that forgets
//! to check `alg` accepts anything.
//!
//! This asks the provider instead. The access token is exchanged over TLS
//! directly with the token endpoint, and `userinfo` is fetched with it over
//! TLS from the same issuer. The claims are the provider's own answer rather
//! than something this file decided a signature meant. OIDC Core §3.1.3.7
//! permits exactly this for tokens received directly from the token endpoint,
//! and it removes a whole class of verification bug rather than implementing
//! one carefully.
//!
//! ## What a provider may not decide
//!
//! It says who somebody is. It does not say what they may do: a new account
//! arrives as a viewer, and an administrator promotes it. An estate where
//! anyone in the company directory can restart production on first login is
//! not a thing to build by accident.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{AppError, Result};

/// How long a sign-in may sit half-finished before its state is discarded.
///
/// Generous enough for somebody to complete a push notification and type a
/// code; short enough that an abandoned attempt is not a credential lying
/// around.
const FLOW_MINUTES: i64 = 10;

/// How this server talks to a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OidcSettings {
    /// `https://accounts.google.com`, `https://your-org.okta.com`. Discovery
    /// happens against this, so nothing else has to be configured by hand.
    pub issuer: String,
    pub client_id: String,
    /// Never returned to a client, and read from the environment in
    /// deployments so it need not sit in a config file.
    #[serde(default, skip_serializing)]
    pub client_secret: String,
    /// Where the provider sends the browser back. Must match what is
    /// registered with the provider exactly, including the scheme.
    pub redirect_uri: String,
}

impl OidcSettings {
    /// Environment first, so a container is configured without editing a file.
    pub fn resolved(&self) -> Option<OidcSettings> {
        let env = |key: &str, fallback: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| fallback.trim().to_string())
        };

        let settings = OidcSettings {
            issuer: env("ROOT_OIDC_ISSUER", &self.issuer),
            client_id: env("ROOT_OIDC_CLIENT_ID", &self.client_id),
            client_secret: env("ROOT_OIDC_CLIENT_SECRET", &self.client_secret),
            redirect_uri: env("ROOT_OIDC_REDIRECT_URI", &self.redirect_uri),
        };

        // All four or none. A half-configured provider produces a sign-in
        // button that fails after the redirect, which is the worst place to
        // find out.
        let complete = ![
            &settings.issuer,
            &settings.client_id,
            &settings.client_secret,
            &settings.redirect_uri,
        ]
        .iter()
        .any(|v| v.is_empty());

        complete.then_some(settings)
    }
}

/// The endpoints a provider advertises.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
}

/// One sign-in, between the redirect out and the redirect back.
#[derive(Debug, Clone)]
struct Pending {
    verifier: String,
    started: DateTime<Utc>,
}

/// A provider this server can sign people in with.
#[derive(Clone)]
pub struct Provider {
    settings: OidcSettings,
    discovery: Discovery,
    client: reqwest::Client,
    /// In flight sign-ins, by `state`.
    ///
    /// In memory, which means a restart mid-sign-in sends somebody back to
    /// the button. That is the right trade for a ten-minute window: the
    /// alternative is a table whose rows are single-use and mostly garbage.
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

impl Provider {
    /// Read the provider's own description of itself.
    pub async fn discover(settings: OidcSettings) -> Result<Self> {
        let client = crate::collectors::http_client(false, std::time::Duration::from_secs(15))?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            settings.issuer.trim_end_matches('/')
        );

        let discovery: Discovery = client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Config(format!("{url} could not be reached: {e}")))?
            .json()
            .await
            .map_err(|e| {
                AppError::Config(format!("{url} is not an OpenID configuration: {e}"))
            })?;

        Ok(Provider {
            settings,
            discovery,
            client,
            pending: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Where to send the browser, and the `state` that will come back.
    pub async fn start(&self) -> (String, String) {
        let state = random();
        let verifier = random();
        let challenge = challenge_for(&verifier);

        {
            let mut pending = self.pending.lock().await;
            // Drop anything abandoned, so a long-lived server does not
            // accumulate the half of these that nobody finishes.
            let cutoff = Utc::now() - Duration::minutes(FLOW_MINUTES);
            pending.retain(|_, flow| flow.started > cutoff);
            pending.insert(
                state.clone(),
                Pending {
                    verifier,
                    started: Utc::now(),
                },
            );
        }

        let url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}\
             &code_challenge={}&code_challenge_method=S256",
            self.discovery.authorization_endpoint,
            urlencoding::encode(&self.settings.client_id),
            urlencoding::encode(&self.settings.redirect_uri),
            urlencoding::encode("openid email profile"),
            urlencoding::encode(&state),
            urlencoding::encode(&challenge),
        );
        (url, state)
    }

    /// Exchange the code for claims about whoever just signed in.
    pub async fn finish(&self, code: &str, state: &str) -> Result<Claims> {
        // Taken, not read: a `state` is single-use, and one that could be
        // replayed is one an attacker can replay.
        let flow = self.pending.lock().await.remove(state);
        let Some(flow) = flow else {
            return Err(AppError::Rejected(
                "that sign-in has expired or was already used. Try again.".into(),
            ));
        };

        let token: TokenResponse = self
            .client
            .post(&self.discovery.token_endpoint)
            // Built by hand rather than with reqwest's `form`, which this
            // crate's feature set does not include. The encoding is the same
            // one the authorize URL above uses.
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.settings.redirect_uri),
                ("client_id", &self.settings.client_id),
                ("client_secret", &self.settings.client_secret),
                ("code_verifier", &flow.verifier),
            ]))
            .send()
            .await
            .map_err(|e| AppError::Rejected(format!("the provider did not answer: {e}")))?
            .json()
            .await
            .map_err(|e| {
                AppError::Rejected(format!("the provider's token response was unreadable: {e}"))
            })?;

        // The provider's own answer about who this is, over TLS, from the
        // issuer we were configured with. See the module note on why this is
        // preferred to parsing the id_token.
        let claims: Claims = self
            .client
            .get(&self.discovery.userinfo_endpoint)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| AppError::Rejected(format!("userinfo could not be read: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::Rejected(format!("userinfo was unreadable: {e}")))?;

        if claims.email.trim().is_empty() {
            return Err(AppError::Rejected(
                "the provider did not return an email address, so there is nobody to sign in"
                    .into(),
            ));
        }
        Ok(claims)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// What the provider says about somebody.
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
}

/// Percent-encode a set of pairs as a form body.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A URL-safe random string, from the OS.
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// S256: the base64url of the SHA-256 of the verifier, unpadded.
fn challenge_for(verifier: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> OidcSettings {
        OidcSettings {
            issuer: "https://issuer.example".into(),
            client_id: "root".into(),
            client_secret: "shh".into(),
            redirect_uri: "https://root.example/auth/oidc/callback".into(),
        }
    }

    #[test]
    fn a_half_configured_provider_is_no_provider() {
        // Otherwise a sign-in button appears and fails after the redirect,
        // which is the worst place to discover a missing client secret.
        for missing in 0..4 {
            let mut settings = complete();
            match missing {
                0 => settings.issuer.clear(),
                1 => settings.client_id.clear(),
                2 => settings.client_secret.clear(),
                _ => settings.redirect_uri.clear(),
            }
            assert!(settings.resolved().is_none(), "field {missing}");
        }
        assert!(complete().resolved().is_some());
    }

    #[test]
    fn the_secret_is_never_serialized() {
        // It would otherwise reach a browser through get_config, which
        // returns the whole settings object.
        let json = serde_json::to_string(&complete()).unwrap();
        assert!(!json.contains("shh"), "{json}");
        assert!(json.contains("issuer.example"));
    }

    #[test]
    fn a_challenge_is_the_hash_and_not_the_verifier() {
        let verifier = "a-verifier";
        let challenge = challenge_for(verifier);
        assert_ne!(challenge, verifier);
        // Unpadded base64url: no '=', '+' or '/'.
        assert!(!challenge.contains('='), "{challenge}");
        assert!(!challenge.contains('+'), "{challenge}");
        assert!(!challenge.contains('/'), "{challenge}");
        assert_eq!(challenge, challenge_for(verifier), "must be stable");
    }

    #[test]
    fn the_challenge_matches_the_rfc_example() {
        // RFC 7636 Appendix B. If this drifts, every provider rejects the
        // exchange with an error that does not mention PKCE.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_form_body_escapes_what_would_otherwise_end_a_field() {
        // A client secret containing & or = is ordinary, and would silently
        // truncate the exchange without this.
        let body = form(&[("code", "a&b"), ("client_secret", "x=y z")]);
        assert_eq!(body, "code=a%26b&client_secret=x%3Dy%20z");
    }

    #[test]
    fn two_sign_ins_never_share_a_secret() {
        let a = random();
        let b = random();
        assert_ne!(a, b);
        assert!(a.len() >= 64);
    }
}
