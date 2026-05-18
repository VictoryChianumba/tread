//! Shared persistence-load helper.
//!
//! Every per-paper JSON file (progress, bookmarks, highlights, config)
//! used to load via `serde_json::from_str(&data).unwrap_or_default()`,
//! which has a quiet failure mode: a partial-write / disk-corruption /
//! schema-mismatch silently returns `Default`, the next `save()`
//! overwrites the file with that default value, and the user loses
//! their marks / highlights / reading position with no signal.
//!
//! `load_json` instead:
//!
//! 1. Returns `Default` when the file is *missing* (the normal first-
//!    run case).
//! 2. On parse failure, renames the file to `<name>.corrupt-<unix-ts>`
//!    BEFORE returning `Default` — the next `save()` will write a
//!    fresh file at the original path, but the user's old data is
//!    preserved at the renamed path for forensic recovery.
//! 3. Prints a one-line warning to stderr so the user has SOME signal
//!    if they're running tread outside the alt screen (or after the
//!    TUI exits and the terminal scrolls back).
//!
//! The eprintln is deliberately terse — production hosts (`trench`)
//! suppress it via stderr redirection anyway; the value is the
//! preserved backup, not the message.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// Read `path` as JSON and parse into `T`.  See module docs for the
/// missing-file / parse-failure semantics.
pub fn load_json<T>(path: &Path) -> T
where
    T: DeserializeOwned + Default,
{
    let Ok(data) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match serde_json::from_str::<T>(&data) {
        Ok(value) => value,
        Err(err) => {
            let backup = corrupt_backup_path(path);
            // Rename failure is itself non-fatal — we still want to
            // return Default so the caller can keep running.  But a
            // rename failure would let the next save() overwrite the
            // original; warn loudly in that case so the user knows
            // their data is in genuine danger.
            match std::fs::rename(path, &backup) {
                Ok(()) => {
                    eprintln!(
                        "tread: {} failed to parse ({err}); \
                         old contents preserved at {}",
                        path.display(),
                        backup.display(),
                    );
                }
                Err(rename_err) => {
                    eprintln!(
                        "tread: {} failed to parse ({err}); \
                         could not move to backup ({rename_err}); \
                         next save will overwrite — copy the file aside if you want it",
                        path.display(),
                    );
                }
            }
            T::default()
        }
    }
}

/// `<dir>/<name>.json` → `<dir>/<name>.json.corrupt-<unix-ts>`.
/// The timestamp ensures repeated parse failures don't clobber prior
/// backups — each gets a unique suffix.  Falls back to `0` if the
/// system clock is somehow before the epoch.
fn corrupt_backup_path(path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let original = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let mut backup = path.to_path_buf();
    backup.set_file_name(format!("{original}.corrupt-{stamp}"));
    backup
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
    struct Sample {
        n: u32,
    }

    /// Fresh tempdir for one test.  Uses pid + a per-test tag so
    /// parallel cargo-test runs (different processes) and parallel
    /// test threads (same process, different tag) can't collide.
    /// Manually cleaned up at end of test via the returned guard.
    struct TestDir(PathBuf);
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fresh_path(tag: &str) -> (TestDir, PathBuf) {
        let dir = std::env::temp_dir()
            .join("tread-persist-test")
            .join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.json");
        (TestDir(dir), path)
    }

    #[test]
    fn missing_file_returns_default_without_writing_anything() {
        let (_guard, path) = fresh_path("missing");
        let value: Sample = load_json(&path);
        assert_eq!(value, Sample::default());
        // No backup should have been created — the original was just absent.
        assert!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().next().is_none(),
            "missing-file case must not touch the filesystem",
        );
    }

    #[test]
    fn valid_file_round_trips() {
        let (_guard, path) = fresh_path("valid");
        std::fs::write(&path, r#"{"n": 42}"#).unwrap();
        let value: Sample = load_json(&path);
        assert_eq!(value, Sample { n: 42 });
    }

    #[test]
    fn corrupt_file_is_renamed_and_default_returned() {
        let (_guard, path) = fresh_path("corrupt");
        std::fs::write(&path, "this is not json {{ {").unwrap();

        let value: Sample = load_json(&path);
        assert_eq!(value, Sample::default(), "load must return Default on parse failure");

        // Original path is now empty (renamed away), and there's a
        // sibling file with the `.corrupt-<ts>` suffix.
        assert!(!path.exists(), "original path should be vacated");
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one backup file should remain");
        let name = entries[0].file_name().into_string().unwrap();
        assert!(
            name.starts_with("sample.json.corrupt-"),
            "backup name should match the corrupt suffix pattern; got {name}",
        );
    }
}
