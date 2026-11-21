//! Captures on disk: one `<id>.json` per capture in the data folder. Plain files, so agents can
//! read them directly, `tail` them or watch the folder.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::capture::{Capture, CaptureId};

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("couldn't {action} {path}: {source}")]
    Io { action: &'static str, path: PathBuf, source: std::io::Error },
    #[error("{path} isn't a valid capture: {source}. The file is untouched; fix or remove it.")]
    Corrupt { path: PathBuf, source: serde_json::Error },
    #[error("no capture matches `{0}`. Run `tin list` to see ids.")]
    NotFound(String),
    #[error("`{query}` matches {count} captures; use more of the id.")]
    Ambiguous { query: String, count: usize },
}

/// Everything in the store, newest first, and any file that couldn't be read.
#[derive(Debug, Default)]
pub struct Listing {
    pub captures: Vec<Capture>,
    pub unreadable: Vec<StoreError>,
}

impl Store {
    /// Opens the store at `dir`, creating the folder if needed.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Store, StoreError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|source| StoreError::Io { action: "create", path: dir.clone(), source })?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, id: &CaptureId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Writes the capture whole: to a temporary file, then renamed over the old one.
    pub fn save(&self, capture: &Capture) -> Result<PathBuf, StoreError> {
        let path = self.path(&capture.id);
        let tmp = self.dir.join(format!(".{}.json.tmp", capture.id));
        let io = |action, source| StoreError::Io { action, path: path.clone(), source };
        let json = serde_json::to_vec_pretty(capture).map_err(|e| io("serialize", std::io::Error::other(e)))?;
        let mut file = fs::File::create(&tmp).map_err(|e| io("write", e))?;
        file.write_all(&json)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .map_err(|e| io("write", e))?;
        fs::rename(&tmp, &path).map_err(|e| io("write", e))?;
        Ok(path)
    }

    /// Writes a capture's window picture beside its JSON, as `<id>.png`.
    pub fn save_screenshot(&self, id: &CaptureId, png: &[u8]) -> Result<PathBuf, StoreError> {
        let path = self.dir.join(format!("{id}.png"));
        let tmp = self.dir.join(format!(".{id}.png.tmp"));
        let io = |source| StoreError::Io { action: "write", path: path.clone(), source };
        fs::write(&tmp, png).map_err(io)?;
        fs::rename(&tmp, &path).map_err(io)?;
        Ok(path)
    }

    pub fn load(&self, id: &CaptureId) -> Result<Capture, StoreError> {
        let path = self.path(id);
        let bytes = fs::read(&path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => StoreError::NotFound(id.to_string()),
            _ => StoreError::Io { action: "read", path: path.clone(), source },
        })?;
        serde_json::from_slice(&bytes).map_err(|source| StoreError::Corrupt { path, source })
    }

    /// Deletes the capture and what's kept beside it (its picture, a kept tweet and its media).
    pub fn remove(&self, id: &CaptureId) -> Result<(), StoreError> {
        for extra in [format!("{id}.png"), format!("{id}.tweet.json")] {
            let path = self.dir.join(extra);
            match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(StoreError::Io { action: "remove", path, source: e });
                }
                _ => {}
            }
        }
        let media = self.dir.join(format!("{id}.media"));
        match fs::remove_dir_all(&media) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(StoreError::Io { action: "remove", path: media, source: e });
            }
            _ => {}
        }
        let path = self.path(id);
        fs::remove_file(&path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => StoreError::NotFound(id.to_string()),
            _ => StoreError::Io { action: "remove", path, source },
        })
    }

    pub fn ids(&self) -> Result<Vec<CaptureId>, StoreError> {
        let entries = fs::read_dir(&self.dir).map_err(|source| StoreError::Io {
            action: "list",
            path: self.dir.clone(),
            source,
        })?;
        let mut ids: Vec<CaptureId> = entries
            .flatten()
            .filter_map(|e| CaptureId::parse(e.file_name().to_str()?.strip_suffix(".json")?).ok())
            .collect();
        ids.sort_by(|a, b| b.cmp(a));
        Ok(ids)
    }

    pub fn list(&self) -> Result<Listing, StoreError> {
        let mut listing = Listing::default();
        for id in self.ids()? {
            match self.load(&id) {
                Ok(capture) => listing.captures.push(capture),
                // Removed between listing and reading: not a problem.
                Err(StoreError::NotFound(_)) => {}
                Err(e) => listing.unreadable.push(e),
            }
        }
        Ok(listing)
    }

    /// A full id, `last` (the newest), or a unique prefix or suffix of one.
    pub fn resolve(&self, query: &str) -> Result<CaptureId, StoreError> {
        let ids = self.ids()?;
        if query == "last" {
            return ids.into_iter().next().ok_or_else(|| StoreError::NotFound(query.to_owned()));
        }
        let matches: Vec<CaptureId> = ids
            .into_iter()
            .filter(|id| !query.is_empty() && (id.as_str().starts_with(query) || id.as_str().ends_with(query)))
            .collect();
        match matches.len() {
            0 => Err(StoreError::NotFound(query.to_owned())),
            1 => Ok(matches.into_iter().next().ok_or_else(|| StoreError::NotFound(query.to_owned()))?),
            count => Err(StoreError::Ambiguous { query: query.to_owned(), count }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::capture::{Draft, Status};

    fn capture(ms: u64, note: &str) -> Capture {
        let draft = Draft { note: Some(note.into()), ..Draft::default() };
        Capture::new(draft, SystemTime::UNIX_EPOCH + Duration::from_millis(ms)).unwrap_or_else(|e| panic!("{e}"))
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let store = Store::open(dir.path().join("captures")).unwrap_or_else(|e| panic!("{e}"));
        (dir, store)
    }

    #[test]
    fn saves_lists_newest_first_and_round_trips() {
        let (_dir, store) = store();
        let (old, new) = (capture(1_000, "old"), capture(2_000, "new"));
        store.save(&old).unwrap_or_else(|e| panic!("{e}"));
        store.save(&new).unwrap_or_else(|e| panic!("{e}"));
        let listing = store.list().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(listing.captures, [new.clone(), old]);
        assert!(listing.unreadable.is_empty());

        let mut done = new.clone();
        done.status = Status::Done { at: "now".into() };
        store.save(&done).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.load(&new.id).ok(), Some(done));
    }

    #[test]
    fn a_corrupt_file_is_reported_not_skipped_silently() {
        let (_dir, store) = store();
        let good = capture(1_000, "good");
        store.save(&good).unwrap_or_else(|e| panic!("{e}"));
        fs::write(store.dir().join("20990101-000000-000-0000.json"), "{oops").unwrap_or_else(|e| panic!("{e}"));
        fs::write(store.dir().join("notes.txt"), "ignored").unwrap_or_else(|e| panic!("{e}"));
        let listing = store.list().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(listing.captures, [good]);
        assert!(matches!(listing.unreadable.as_slice(), [StoreError::Corrupt { .. }]));
    }

    #[test]
    fn resolves_last_prefixes_and_suffixes() {
        let (_dir, store) = store();
        let (a, b) = (capture(1_000, "a"), capture(61_000, "b"));
        store.save(&a).unwrap_or_else(|e| panic!("{e}"));
        store.save(&b).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.resolve("last").ok(), Some(b.id.clone()));
        assert_eq!(store.resolve(&a.id.as_str()[..19]).ok(), Some(a.id.clone()));
        assert_eq!(store.resolve(&b.id.as_str()[20..]).ok(), Some(b.id.clone()));
        assert!(matches!(store.resolve("1970"), Err(StoreError::Ambiguous { count: 2, .. })));
        assert!(matches!(store.resolve("nope"), Err(StoreError::NotFound(_))));
    }
}
