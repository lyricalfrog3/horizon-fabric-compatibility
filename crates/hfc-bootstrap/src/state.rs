//! `HorizonFabricCompat/state.json`: what HFC installed last time.
//!
//! State is a convenience for cleanup and `hfc status`, never a source of
//! truth: generated folders carry their own marker, and every run rebuilds
//! the desired set from the jars it actually sees.

use hfc_core::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const STATE_FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub format: u32,
    pub installed: Vec<InstalledMod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledMod {
    pub id: String,
    pub source_file: String,
    pub source_sha256: String,
    pub verdict: String,
    pub installed_unix: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            format: STATE_FORMAT,
            installed: Vec::new(),
        }
    }
}

impl State {
    /// Load state, tolerating an absent file. A malformed file is reported and
    /// treated as empty; generated folders are still cleaned up by their own
    /// markers.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("state.json could not be read: {e}")),
                );
            }
        };
        match serde_json::from_slice::<State>(&bytes) {
            Ok(state) if state.format == STATE_FORMAT => (state, None),
            Ok(state) => (
                Self::default(),
                Some(format!(
                    "state.json format {} is not {STATE_FORMAT}; starting fresh",
                    state.format
                )),
            ),
            Err(e) => (
                Self::default(),
                Some(format!("state.json is malformed ({e}); starting fresh")),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        hfc_core::atomic_write_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hfc-state-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_tolerates_damage() {
        let dir = temp("roundtrip");
        let path = dir.join("state.json");
        let state = State {
            format: STATE_FORMAT,
            installed: vec![InstalledMod {
                id: "ruby".into(),
                source_file: "ruby.jar".into(),
                source_sha256: "abc".into(),
                verdict: "works".into(),
                installed_unix: 1,
            }],
        };
        state.save(&path).unwrap();
        let (loaded, note) = State::load(&path);
        assert_eq!(loaded, state);
        assert!(note.is_none());
        std::fs::write(&path, b"not json").unwrap();
        let (loaded, note) = State::load(&path);
        assert!(loaded.installed.is_empty());
        assert!(note.unwrap().contains("malformed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_state_is_empty_and_quiet() {
        let dir = temp("absent");
        let (state, note) = State::load(&dir.join("state.json"));
        assert!(state.installed.is_empty());
        assert!(note.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
