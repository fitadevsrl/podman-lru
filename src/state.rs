//! Persisted last-used timestamps, one per image ID, as a small JSON file.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::model::bare_id;

/// Unix time in seconds.
pub type Timestamp = u64;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    /// Image ID (bare hex) to the last time it was used.
    #[serde(default)]
    pub last_used: HashMap<String, Timestamp>,
    /// Time of the last event handled, so the stream can resume on restart.
    #[serde(default)]
    pub last_event: Option<Timestamp>,
}

impl State {
    /// Load the state file. A missing file is an empty state; a corrupt one is
    /// discarded with a warning rather than blocking startup.
    pub fn load(path: &Path) -> State {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(state) => state,
                Err(err) => {
                    warn!(path = %path.display(), %err, "state file unreadable, starting empty");
                    State::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(err) => {
                warn!(path = %path.display(), %err, "state file unreadable, starting empty");
                State::default()
            }
        }
    }

    /// Write atomically: temp file in the same directory, then rename over.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("state.json")
        ));
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Timestamp> {
        self.last_used.get(bare_id(id)).copied()
    }

    /// Record a use; only ever moves the timestamp forward.
    pub fn touch(&mut self, id: &str, now: Timestamp) -> bool {
        let entry = self.last_used.entry(bare_id(id).to_owned()).or_insert(0);
        if *entry < now {
            *entry = now;
            true
        } else {
            false
        }
    }

    pub fn forget(&mut self, id: &str) -> bool {
        self.last_used.remove(bare_id(id)).is_some()
    }
}

pub fn now() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let mut state = State::default();
        state.touch("sha256:abc", 10);
        state.last_event = Some(11);
        state.save(&path).unwrap();
        let loaded = State::load(&path);
        assert_eq!(loaded, state);
        assert_eq!(loaded.get("abc"), Some(10));
        assert!(!dir.path().join("nested").join(".state.json.tmp").exists());
    }

    #[test]
    fn missing_and_corrupt_files_start_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(&dir.path().join("none.json")), State::default());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert_eq!(State::load(&bad), State::default());
    }

    #[test]
    fn touch_only_moves_forward() {
        let mut state = State::default();
        assert!(state.touch("a", 5));
        assert!(!state.touch("a", 3));
        assert_eq!(state.get("a"), Some(5));
        assert!(state.forget("sha256:a"));
        assert!(!state.forget("a"));
    }
}
