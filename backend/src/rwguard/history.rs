//! Persistent validation history for RWGuard (`rwguard-history.json` in the
//! launcher config dir).
//!
//! Kept separate from `store.json` so tracking 11,000+ validated cache models
//! never bloats or slows down unrelated launcher store writes. Files whose
//! `(size, modified_ms)` match their stored entry are skipped in O(1) without
//! opening them on disk.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "rwguard-history.json";
pub const SCHEMA_VERSION: u32 = 1;
/// Bump if RWGuard detection rules change so previously validated models are
/// automatically re-analyzed on next launch.
pub const SCANNER_VERSION: u32 = 1;
const MAX_BLOCKED_RECORDS: usize = 200;

/// Verdict recorded for a model file present on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredVerdict {
    Safe,
    Healed,
    Threat,
}

/// Compact per-file validation record. Short field names keep 11,000+ entries
/// small (~650 KB) and fast to deserialize/serialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// File size in bytes when validated.
    #[serde(rename = "s")]
    pub size: u64,
    /// Last-modified timestamp in milliseconds since UNIX epoch (0 if unknown).
    #[serde(rename = "m")]
    pub modified_ms: u64,
    /// Validation verdict.
    #[serde(rename = "v")]
    pub verdict: StoredVerdict,
    /// Threat detail when `verdict == StoredVerdict::Threat`.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// UNIX timestamp (seconds) when RWGuard validated this file.
    #[serde(rename = "t")]
    pub checked_at: u64,
}

impl HistoryEntry {
    /// Returns `true` when the on-disk file still matches this validation record.
    pub fn matches(&self, size: u64, modified_ms: u64) -> bool {
        self.size == size && self.modified_ms == modified_ms
    }
}

/// A model rejected by RWGuard during download or package extraction before
/// being placed in the cache folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedRecord {
    pub file_name: String,
    pub size: u64,
    pub reason: String,
    pub blocked_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderHistory {
    #[serde(default)]
    pub files: BTreeMap<String, HistoryEntry>,
    #[serde(default)]
    pub blocked: Vec<BlockedRecord>,
    #[serde(default)]
    pub last_scan_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    schema_version: u32,
    scanner_version: u32,
    #[serde(default)]
    folders: BTreeMap<String, FolderHistory>,
}

impl Default for HistoryFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            scanner_version: SCANNER_VERSION,
            folders: BTreeMap::new(),
        }
    }
}

struct Inner {
    data: HistoryFile,
    dirty: bool,
    pending_updates: usize,
}

/// Thread-safe persistent store of RWGuard validation records.
pub struct RwGuardHistory {
    path: PathBuf,
    inner: Mutex<Inner>,
}

/// Normalizes a folder path into a stable case-insensitive map key.
pub fn folder_key(dir: &Path) -> String {
    let raw = dir.to_string_lossy();
    raw.trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn modified_millis(time: Option<SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl RwGuardHistory {
    /// Opens `path`, resetting to empty if missing, corrupt, or written by an
    /// incompatible schema/scanner version.
    pub fn open(path: PathBuf) -> Self {
        let data = match fs::read(&path) {
            Ok(bytes) => {
                let slice = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
                match serde_json::from_slice::<HistoryFile>(slice) {
                    Ok(file)
                        if file.schema_version == SCHEMA_VERSION
                            && file.scanner_version == SCANNER_VERSION =>
                    {
                        file
                    }
                    Ok(file) => {
                        log::info!(
                            "rwguard history version changed (schema {}, scanner {}); re-validating cache",
                            file.schema_version,
                            file.scanner_version
                        );
                        HistoryFile::default()
                    }
                    Err(error) => {
                        log::warn!(
                            "rwguard history at {} is corrupt, starting fresh: {error}",
                            path.display()
                        );
                        HistoryFile::default()
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => HistoryFile::default(),
            Err(error) => {
                log::warn!(
                    "could not read rwguard history at {}: {error}",
                    path.display()
                );
                HistoryFile::default()
            }
        };
        Self {
            path,
            inner: Mutex::new(Inner {
                data,
                dirty: false,
                pending_updates: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Snapshot of a folder's history.
    pub fn folder_snapshot(&self, dir_key: &str) -> FolderHistory {
        self.lock()
            .data
            .folders
            .get(dir_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Commits a batch of newly validated entries, optionally pruning keys that
    /// no longer exist in `present_names`, updates `last_scan_at`, and persists
    /// atomically only if anything changed.
    pub fn commit_scan(
        &self,
        dir_key: &str,
        updates: Vec<(String, HistoryEntry)>,
        present_names: Option<&HashSet<String>>,
        scan_timestamp: u64,
    ) {
        let mut inner = self.lock();
        let folder = inner.data.folders.entry(dir_key.to_string()).or_default();
        let mut changed = false;

        for (name, entry) in updates {
            let same = folder.files.get(&name).is_some_and(|old| old == &entry);
            if !same {
                folder.files.insert(name, entry);
                changed = true;
            }
        }

        if let Some(present) = present_names {
            let before = folder.files.len();
            folder.files.retain(|name, _| present.contains(name));
            if folder.files.len() != before {
                changed = true;
            }
        }

        let had_scan = folder.last_scan_at.is_some();
        folder.last_scan_at = Some(scan_timestamp);
        if !had_scan {
            changed = true;
        }

        if changed {
            inner.dirty = true;
            inner.pending_updates = 0;
            self.write_locked(&mut inner);
        }
    }

    /// Records a single validated model during concurrent downloads/extraction.
    /// Flushes to disk every `flush_every` updates so crashes lose minimal work
    /// without thrashing the filesystem on every individual model.
    pub fn record_one(&self, dir_key: &str, name: String, entry: HistoryEntry, flush_every: usize) {
        let mut inner = self.lock();
        let folder = inner.data.folders.entry(dir_key.to_string()).or_default();
        // If a previously blocked model is now clean on disk, drop the stale
        // blocked notice for it.
        if entry.verdict != StoredVerdict::Threat {
            folder.blocked.retain(|b| b.file_name != name);
        }
        folder.files.insert(name, entry);
        inner.dirty = true;
        inner.pending_updates += 1;
        if flush_every > 0 && inner.pending_updates >= flush_every {
            inner.pending_updates = 0;
            self.write_locked(&mut inner);
        }
    }

    /// Records a model blocked during download/extraction and persists immediately.
    pub fn record_blocked(&self, dir_key: &str, record: BlockedRecord) {
        let mut inner = self.lock();
        let folder = inner.data.folders.entry(dir_key.to_string()).or_default();
        folder.blocked.retain(|b| b.file_name != record.file_name);
        folder.blocked.insert(0, record);
        if folder.blocked.len() > MAX_BLOCKED_RECORDS {
            folder.blocked.truncate(MAX_BLOCKED_RECORDS);
        }
        inner.dirty = true;
        inner.pending_updates = 0;
        self.write_locked(&mut inner);
    }

    /// Removes one or more deleted files from history and persists if needed.
    pub fn remove_files(&self, dir_key: &str, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let Some(folder) = inner.data.folders.get_mut(dir_key) else {
            return;
        };
        let mut changed = false;
        for name in names {
            if folder.files.remove(name).is_some() {
                changed = true;
            }
        }
        if changed {
            inner.dirty = true;
            inner.pending_updates = 0;
            self.write_locked(&mut inner);
        }
    }

    /// Clears recorded blocked-on-download notices for `dir_key`.
    pub fn clear_blocked(&self, dir_key: &str, file_name: Option<&str>) -> usize {
        let mut inner = self.lock();
        let Some(folder) = inner.data.folders.get_mut(dir_key) else {
            return 0;
        };
        let before = folder.blocked.len();
        match file_name {
            Some(target) => folder.blocked.retain(|b| b.file_name != target),
            None => folder.blocked.clear(),
        }
        let removed = before - folder.blocked.len();
        if removed > 0 {
            inner.dirty = true;
            inner.pending_updates = 0;
            self.write_locked(&mut inner);
        }
        removed
    }

    /// Persists pending in-memory changes atomically.
    pub fn flush(&self) {
        let mut inner = self.lock();
        if inner.dirty {
            inner.pending_updates = 0;
            self.write_locked(&mut inner);
        }
    }

    fn write_locked(&self, inner: &mut Inner) {
        match write_atomic(&self.path, &inner.data) {
            Ok(()) => inner.dirty = false,
            Err(error) => {
                log::error!(
                    "could not save rwguard history to {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

fn write_atomic(path: &Path, data: &HistoryFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec(data).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(size: u64, modified_ms: u64, verdict: StoredVerdict) -> HistoryEntry {
        HistoryEntry {
            size,
            modified_ms,
            verdict,
            reason: (verdict == StoredVerdict::Threat).then(|| "Atomic STRUCT overflow".into()),
            checked_at: 1_700_000_000,
        }
    }

    #[test]
    fn round_trips_and_prunes_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        let key = folder_key(Path::new(r"C:\Cache\135.148.128.72.7777\"));

        let hist = RwGuardHistory::open(path.clone());
        hist.commit_scan(
            &key,
            vec![
                (
                    "0xA.dff".into(),
                    sample_entry(100, 1000, StoredVerdict::Safe),
                ),
                (
                    "0xB.txd".into(),
                    sample_entry(200, 2000, StoredVerdict::Threat),
                ),
            ],
            None,
            1_700_000_100,
        );

        let reopened = RwGuardHistory::open(path.clone());
        let snap = reopened.folder_snapshot(&key);
        assert_eq!(snap.files.len(), 2);
        assert!(snap.files["0xA.dff"].matches(100, 1000));
        assert!(!snap.files["0xA.dff"].matches(101, 1000));
        assert!(!snap.files["0xA.dff"].matches(100, 1001));
        assert_eq!(snap.last_scan_at, Some(1_700_000_100));

        // Prune 0xB.txd when only 0xA.dff remains on disk.
        let present: HashSet<String> = ["0xA.dff".to_string()].into_iter().collect();
        reopened.commit_scan(&key, Vec::new(), Some(&present), 1_700_000_200);
        let snap = RwGuardHistory::open(path).folder_snapshot(&key);
        assert_eq!(snap.files.len(), 1);
        assert!(snap.files.contains_key("0xA.dff"));
    }

    #[test]
    fn version_mismatch_or_corrupt_file_resets_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        fs::write(&path, b"{ broken json").unwrap();
        let hist = RwGuardHistory::open(path.clone());
        assert!(hist.folder_snapshot("x").files.is_empty());

        fs::write(
            &path,
            br#"{"schemaVersion":1,"scannerVersion":999,"folders":{"x":{"files":{"0xA.dff":{"s":1,"m":1,"v":"safe","t":1}}}}}"#,
        )
        .unwrap();
        let hist = RwGuardHistory::open(path);
        assert!(hist.folder_snapshot("x").files.is_empty());
    }

    #[test]
    fn blocked_records_deduplicate_and_clear_when_clean_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let hist = RwGuardHistory::open(tmp.path().join(FILE_NAME));
        hist.record_blocked(
            "k",
            BlockedRecord {
                file_name: "0xBAD.dff".into(),
                size: 500,
                reason: "Atomic STRUCT overflow".into(),
                blocked_at: 10,
            },
        );
        hist.record_blocked(
            "k",
            BlockedRecord {
                file_name: "0xBAD.dff".into(),
                size: 500,
                reason: "Atomic STRUCT overflow".into(),
                blocked_at: 20,
            },
        );
        let snap = hist.folder_snapshot("k");
        assert_eq!(snap.blocked.len(), 1);
        assert_eq!(snap.blocked[0].blocked_at, 20);

        // Once a clean file with the same name lands, the blocked notice clears.
        hist.record_one(
            "k",
            "0xBAD.dff".into(),
            sample_entry(500, 30, StoredVerdict::Safe),
            1,
        );
        assert!(hist.folder_snapshot("k").blocked.is_empty());
    }
}
