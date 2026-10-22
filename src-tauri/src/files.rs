//! Reading declared files wherever they live.
//!
//! A connection's configuration and its logs are declared as a path and a
//! place. Every place answers the same three questions — which files does this
//! path name, how big is this one and is it still the same file, and what are
//! its bytes from here on — and everything above this module asks only those.
//! A new place is one more implementation, and none of them can write.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::{AppError, Result};

/// Most files one declared path may name, so a pattern over a big tree is bounded.
pub const MAX_FILES: usize = 200;

/// Where a declared path is read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Location {
    /// The machine root runs on.
    #[default]
    Local,
}

/// One file, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    /// Changes when the path starts naming a different file, as a rotated log
    /// does: the inode, where there is one.
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
}

/// The reader for a place.
pub async fn reader(_state: &crate::state::AppState, location: &Location) -> Result<Box<dyn Reader>> {
    match location {
        Location::Local => Ok(Box::new(Local)),
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
}
