//! Credential storage backed by the OS keychain.
//!
//! The keyring crate is synchronous and can block (macOS may show an unlock
//! prompt), so every call is pushed onto a blocking thread.

use keyring::Entry;

use crate::error::{AppError, Result};

// Keep the original namespace after the bundle-id correction so existing
// keychain entries remain available without reading or rewriting secrets.
const SERVICE: &str = "com.inframonitor.app";

fn entry(key: &str) -> Result<Entry> {
    Entry::new(SERVICE, key).map_err(|e| AppError::Secret(e.to_string()))
}

pub async fn set_secret(key: &str, value: &str) -> Result<()> {
    let key = key.to_string();
    let value = value.to_string();
    tokio::task::spawn_blocking(move || {
        entry(&key)?
            .set_password(&value)
            .map_err(|e| AppError::Secret(e.to_string()))
    })
    .await
    .map_err(|e| AppError::Secret(e.to_string()))?
}

/// Returns `None` when no credential is stored, rather than treating a missing
/// entry as an error — an unconfigured connection is a normal state.
pub async fn get_secret(key: &str) -> Result<Option<String>> {
    let key = key.to_string();
    tokio::task::spawn_blocking(move || match entry(&key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Secret(e.to_string())),
    })
    .await
    .map_err(|e| AppError::Secret(e.to_string()))?
}

pub async fn delete_secret(key: &str) -> Result<()> {
    let key = key.to_string();
    tokio::task::spawn_blocking(move || match entry(&key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Secret(e.to_string())),
    })
    .await
    .map_err(|e| AppError::Secret(e.to_string()))?
}

/// True when the platform keychain came up. If this is false the app still
/// runs, but connections needing credentials will fail with a clear message.
pub fn store_available() -> bool {
    Entry::store_status().is_ok()
}
