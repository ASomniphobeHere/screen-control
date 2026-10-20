//! Load and save JSON files. Saves are safe against power cuts:
//! write a temp file, flush it to disk, then rename it over the old file.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::core::state::State;

/// Returns `None` if the file does not exist.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let value =
        serde_json::from_str(&text).with_context(|| format!("bad JSON in {}", path.display()))?;
    Ok(Some(value))
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    save_text(path, &text)
}

pub fn save_text(path: &Path, text: &str) -> Result<()> {
    let tmp = tmp_path(path);
    {
        let mut file =
            File::create(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Loads the state. A missing file gives empty state. A broken file is kept as
/// `state.json.broken` for checking later, and empty state is used.
pub fn load_state(path: &Path) -> State {
    match load_json(path) {
        Ok(Some(state)) => state,
        Ok(None) => {
            tracing::info!(path = %path.display(), "no state file, starting with empty state");
            State::default()
        }
        Err(e) => {
            let broken = path.with_extension("json.broken");
            tracing::error!(error = format!("{e:#}"), kept_as = %broken.display(), "state file is broken, starting with empty state");
            if let Err(e) = fs::rename(path, &broken) {
                tracing::error!(error = %e, "cannot keep the broken state file");
            }
            State::default()
        }
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("screen-control-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_gives_same_state() {
        let dir = test_dir("roundtrip");
        let path = dir.join("state.json");
        let mut state = State::default();
        state.roll_day(chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        state.child_mut("janis").used_ms = 1234;
        state.child_mut("janis").warned.insert(5);

        save_json(&path, &state).unwrap();
        save_json(&path, &state).unwrap(); // replacing an existing file works
        assert_eq!(load_state(&path), state);
        assert!(!tmp_path(&path).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_gives_empty_state() {
        let dir = test_dir("missing");
        assert_eq!(load_state(&dir.join("state.json")), State::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_file_gives_empty_state_and_is_kept() {
        let dir = test_dir("broken");
        let path = dir.join("state.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_state(&path), State::default());
        assert!(dir.join("state.json.broken").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
