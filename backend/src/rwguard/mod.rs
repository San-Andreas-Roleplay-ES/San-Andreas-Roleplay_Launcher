//! Native RWGuard integration: validates RenderWare models (`.dff` / `.txd`)
//! in the server cache folder and concurrently as models download or unpack,
//! backed by a persistent validation history so unchanged files are never
//! re-scanned.

pub mod history;
pub mod scanner;

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::app::events::{self, RWGUARD_PROGRESS, RWGUARD_STATUS_CHANGED};
use crate::game::launcher::find_game_process;
use crate::install::cache::{cache_root, with_writable};
use crate::Ctx;

use self::history::{
    folder_key, modified_millis, now_unix_secs, BlockedRecord, HistoryEntry, RwGuardHistory,
    StoredVerdict,
};
use self::scanner::{heal_file, is_model_name, scan_file, scan_file_as, ScanVerdict};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(120);
const RECENT_HISTORY_LIMIT: usize = 60;
const STREAM_FLUSH_EVERY: usize = 200;

/// Threat found in the cache folder or blocked before landing in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RwGuardThreat {
    pub file_name: String,
    pub size: u64,
    pub reason: String,
    /// `"cache"` (file currently on disk) or `"download"` (blocked in flight).
    pub source: &'static str,
    pub detected_at: u64,
}

/// Entry shown in the UI's recent validation history list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RwGuardRecord {
    pub file_name: String,
    pub size: u64,
    /// `"safe"`, `"healed"`, `"threat"`, or `"blocked"`.
    pub verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub checked_at: u64,
}

/// Live progress emitted over `rwguard:progress` during scans and concurrent
/// download validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RwGuardProgress {
    pub scanning: bool,
    /// Models in the current scan batch (0 during live download streaming).
    pub total: usize,
    pub checked: usize,
    pub reused: usize,
    pub newly_scanned: usize,
    pub threats: usize,
    pub blocked: usize,
    /// Models validated live during an ongoing cache download/extraction.
    pub live_validated: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Snapshot returned by `rwguard_status` / `rwguard_scan` and emitted over
/// `rwguard:status:changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RwGuardStatus {
    pub total_models: usize,
    pub validated_models: usize,
    pub reused_from_history: usize,
    pub newly_scanned: usize,
    pub healed_models: usize,
    pub active_threats: Vec<RwGuardThreat>,
    pub blocked_threats: Vec<RwGuardThreat>,
    pub recent_history: Vec<RwGuardRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<u64>,
    pub scanning: bool,
}

/// Result of healing or deleting detected threats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RwGuardActionResult {
    pub ok: bool,
    pub affected: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub status: RwGuardStatus,
}

/// Summary of an incremental directory scan.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanSummary {
    pub total: usize,
    pub reused: usize,
    pub newly_scanned: usize,
    pub threats: usize,
}

#[derive(Default, Clone, Copy)]
struct LastRunMetrics {
    reused: usize,
    newly_scanned: usize,
}

/// Shared RWGuard state held in `Ctx`.
pub struct RwGuardState {
    history: Arc<RwGuardHistory>,
    scanning: AtomicBool,
    last_run: Mutex<LastRunMetrics>,
}

impl RwGuardState {
    pub fn new(history: RwGuardHistory) -> Self {
        Self {
            history: Arc::new(history),
            scanning: AtomicBool::new(false),
            last_run: Mutex::new(LastRunMetrics::default()),
        }
    }

    pub fn history(&self) -> Arc<RwGuardHistory> {
        self.history.clone()
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::SeqCst)
    }

    fn lock_metrics(&self) -> MutexGuard<'_, LastRunMetrics> {
        self.last_run
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_last_run(&self, reused: usize, newly_scanned: usize) {
        let mut m = self.lock_metrics();
        m.reused = reused;
        m.newly_scanned = newly_scanned;
    }

    /// Builds a `RwGuardStatus` snapshot for `dir` from persistent history +
    /// last-run counters.
    pub fn snapshot(&self, dir: &Path) -> RwGuardStatus {
        let key = folder_key(dir);
        let folder = self.history.folder_snapshot(&key);
        let metrics = *self.lock_metrics();

        let total_models = folder.files.len();
        let mut validated_models = 0usize;
        let mut healed_models = 0usize;
        let mut active_threats = Vec::new();
        let mut records: Vec<RwGuardRecord> =
            Vec::with_capacity(folder.files.len() + folder.blocked.len());

        for (file_name, entry) in &folder.files {
            match entry.verdict {
                StoredVerdict::Safe => {
                    validated_models += 1;
                    records.push(RwGuardRecord {
                        file_name: file_name.clone(),
                        size: entry.size,
                        verdict: "safe",
                        reason: None,
                        checked_at: entry.checked_at,
                    });
                }
                StoredVerdict::Healed => {
                    validated_models += 1;
                    healed_models += 1;
                    records.push(RwGuardRecord {
                        file_name: file_name.clone(),
                        size: entry.size,
                        verdict: "healed",
                        reason: None,
                        checked_at: entry.checked_at,
                    });
                }
                StoredVerdict::Threat => {
                    let reason = entry
                        .reason
                        .clone()
                        .unwrap_or_else(|| "RenderWare STRUCT overflow".into());
                    active_threats.push(RwGuardThreat {
                        file_name: file_name.clone(),
                        size: entry.size,
                        reason: reason.clone(),
                        source: "cache",
                        detected_at: entry.checked_at,
                    });
                    records.push(RwGuardRecord {
                        file_name: file_name.clone(),
                        size: entry.size,
                        verdict: "threat",
                        reason: Some(reason),
                        checked_at: entry.checked_at,
                    });
                }
            }
        }

        let blocked_threats: Vec<RwGuardThreat> = folder
            .blocked
            .iter()
            .map(|b| {
                records.push(RwGuardRecord {
                    file_name: b.file_name.clone(),
                    size: b.size,
                    verdict: "blocked",
                    reason: Some(b.reason.clone()),
                    checked_at: b.blocked_at,
                });
                RwGuardThreat {
                    file_name: b.file_name.clone(),
                    size: b.size,
                    reason: b.reason.clone(),
                    source: "download",
                    detected_at: b.blocked_at,
                }
            })
            .collect();

        active_threats.sort_by(|a, b| a.file_name.cmp(&b.file_name));

        // Sort most-recently validated first; break ties by filename so UI
        // ordering stays deterministic.
        records.sort_by(|a, b| {
            b.checked_at
                .cmp(&a.checked_at)
                .then_with(|| a.file_name.cmp(&b.file_name))
        });
        records.truncate(RECENT_HISTORY_LIMIT);

        RwGuardStatus {
            total_models,
            validated_models,
            reused_from_history: metrics.reused.min(total_models),
            newly_scanned: metrics.newly_scanned,
            healed_models,
            active_threats,
            blocked_threats,
            recent_history: records,
            last_scan_at: folder.last_scan_at,
            scanning: self.is_scanning(),
        }
    }
}

/// Live validator attached to `cache::Job` so existing cache models are
/// verified first and newly downloaded/extracted models are validated
/// concurrently before landing in the cache folder.
pub struct CacheGuard {
    history: Arc<RwGuardHistory>,
    dir_key: String,
    on_progress: Arc<dyn Fn(RwGuardProgress) + Send + Sync>,
    reused: AtomicUsize,
    preexisting_scanned: AtomicUsize,
    preexisting_threats: AtomicUsize,
    live_validated: AtomicUsize,
    blocked: AtomicUsize,
    blocked_names: Mutex<HashSet<String>>,
    last_emit: Mutex<Option<Instant>>,
}

impl CacheGuard {
    pub fn new(
        history: Arc<RwGuardHistory>,
        dir: &Path,
        on_progress: Arc<dyn Fn(RwGuardProgress) + Send + Sync>,
    ) -> Self {
        Self {
            history,
            dir_key: folder_key(dir),
            on_progress,
            reused: AtomicUsize::new(0),
            preexisting_scanned: AtomicUsize::new(0),
            preexisting_threats: AtomicUsize::new(0),
            live_validated: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
            blocked_names: Mutex::new(HashSet::new()),
            last_emit: Mutex::new(None),
        }
    }

    pub fn was_blocked(&self, name: &str) -> bool {
        self.blocked_names
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(name)
    }

    /// Step 1 of a cache sync: validates any unvalidated/modified models
    /// already present in the server cache folder, skipping unchanged models
    /// recorded in persistent history.
    pub fn scan_existing(&self, dir: &Path) -> io::Result<ScanSummary> {
        let summary = scan_dir_incremental(&self.history, dir, false, Some(&*self.on_progress))?;
        self.reused.store(summary.reused, Ordering::Relaxed);
        self.preexisting_scanned
            .store(summary.newly_scanned, Ordering::Relaxed);
        self.preexisting_threats
            .store(summary.threats, Ordering::Relaxed);
        Ok(summary)
    }

    /// Step 2 (concurrent): validates a freshly downloaded or extracted temp
    /// model before it is renamed into the cache folder.
    ///
    /// Returns `Err(threat_reason)` when RWGuard detects a RenderWare overflow,
    /// recording the block immediately so the caller discards the temp file.
    pub fn validate_temp(
        &self,
        temp_path: &Path,
        logical_name: &str,
        size: u64,
    ) -> Result<(), String> {
        if !is_model_name(logical_name) {
            return Ok(());
        }
        match scan_file_as(temp_path, logical_name) {
            ScanVerdict::Threat(reason) => {
                log::warn!("RWGuard blocked {logical_name}: {reason}");
                self.blocked.fetch_add(1, Ordering::Relaxed);
                self.blocked_names
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(logical_name.to_string());
                self.history.record_blocked(
                    &self.dir_key,
                    BlockedRecord {
                        file_name: logical_name.to_string(),
                        size,
                        reason: reason.clone(),
                        blocked_at: now_unix_secs(),
                    },
                );
                self.emit_live(true);
                Err(reason)
            }
            ScanVerdict::Safe | ScanVerdict::Error => Ok(()),
        }
    }

    /// Records a model that passed validation and was renamed into `final_path`.
    pub fn record_landed(&self, final_path: &Path, logical_name: &str, size: u64) {
        if !is_model_name(logical_name) {
            return;
        }
        let modified_ms = fs::symlink_metadata(final_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| modified_millis(Some(t)))
            .unwrap_or(0);
        self.history.record_one(
            &self.dir_key,
            logical_name.to_string(),
            HistoryEntry {
                size,
                modified_ms,
                verdict: StoredVerdict::Safe,
                reason: None,
                checked_at: now_unix_secs(),
            },
            STREAM_FLUSH_EVERY,
        );
        self.live_validated.fetch_add(1, Ordering::Relaxed);
        self.emit_live(false);
    }

    /// Drops deleted stale cache files from the persistent history.
    pub fn remove_stale(&self, names: &[String]) {
        self.history.remove_files(&self.dir_key, names);
    }

    /// Flushes all accumulated history entries to disk and emits a final event.
    pub fn finish(&self) -> (usize, usize, usize, usize) {
        self.history.flush();
        self.emit_live(true);
        let reused = self.reused.load(Ordering::Relaxed);
        let newly = self.preexisting_scanned.load(Ordering::Relaxed)
            + self.live_validated.load(Ordering::Relaxed);
        let live = self.live_validated.load(Ordering::Relaxed);
        let blocked = self.blocked.load(Ordering::Relaxed);
        (reused, newly, live, blocked)
    }

    fn emit_live(&self, force: bool) {
        let now = Instant::now();
        {
            let mut last = self
                .last_emit
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !force && last.is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE) {
                return;
            }
            *last = Some(now);
        }
        let reused = self.reused.load(Ordering::Relaxed);
        let pre = self.preexisting_scanned.load(Ordering::Relaxed);
        let live = self.live_validated.load(Ordering::Relaxed);
        let threats = self.preexisting_threats.load(Ordering::Relaxed);
        let blocked = self.blocked.load(Ordering::Relaxed);
        (self.on_progress)(RwGuardProgress {
            scanning: false,
            total: 0,
            checked: live,
            reused,
            newly_scanned: pre + live,
            threats,
            blocked,
            live_validated: live,
            message: None,
        });
    }
}

struct DiskModel {
    name: String,
    path: PathBuf,
    size: u64,
    modified_ms: u64,
}

fn list_models(dir: &Path) -> io::Result<Vec<DiskModel>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut models = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str() else {
            continue;
        };
        if !is_model_name(name) {
            continue;
        }
        let meta = entry.metadata()?;
        models.push(DiskModel {
            name: name.to_string(),
            path: entry.path(),
            size: meta.len(),
            modified_ms: modified_millis(meta.modified().ok()),
        });
    }
    Ok(models)
}

/// Scans `dir` incrementally using `history`. Unchanged models (matching size
/// and mtime) are skipped in O(1) unless `force` is `true`. New or modified
/// models are analyzed across a bounded worker thread pool.
pub fn scan_dir_incremental(
    history: &RwGuardHistory,
    dir: &Path,
    force: bool,
    on_progress: Option<&(dyn Fn(RwGuardProgress) + Send + Sync)>,
) -> io::Result<ScanSummary> {
    let models = list_models(dir)?;
    let key = folder_key(dir);
    let existing = history.folder_snapshot(&key);
    let now = now_unix_secs();

    let mut present_names = HashSet::with_capacity(models.len());
    let mut to_scan = Vec::new();
    let mut reused = 0usize;
    let mut threats = 0usize;

    for model in models {
        present_names.insert(model.name.clone());
        if !force {
            if let Some(entry) = existing.files.get(&model.name) {
                if entry.matches(model.size, model.modified_ms) {
                    reused += 1;
                    if entry.verdict == StoredVerdict::Threat {
                        threats += 1;
                    }
                    continue;
                }
            }
        }
        to_scan.push(model);
    }

    let total = present_names.len();
    let to_scan_len = to_scan.len();

    if to_scan_len == 0 {
        history.commit_scan(&key, Vec::new(), Some(&present_names), now);
        if let Some(emit) = on_progress {
            emit(RwGuardProgress {
                scanning: false,
                total,
                checked: total,
                reused,
                newly_scanned: 0,
                threats,
                blocked: existing.blocked.len(),
                live_validated: 0,
                message: None,
            });
        }
        return Ok(ScanSummary {
            total,
            reused,
            newly_scanned: 0,
            threats,
        });
    }

    if let Some(emit) = on_progress {
        emit(RwGuardProgress {
            scanning: true,
            total: to_scan_len,
            checked: 0,
            reused,
            newly_scanned: 0,
            threats,
            blocked: existing.blocked.len(),
            live_validated: 0,
            message: Some(format!(
                "RWGuard: analizando 0 de {to_scan_len} modelos nuevos..."
            )),
        });
    }

    let checked_counter = AtomicUsize::new(0);
    let new_threats = AtomicUsize::new(0);
    let next_index = AtomicUsize::new(0);
    let last_emit = Mutex::new(Instant::now());
    let updates: Mutex<Vec<(String, HistoryEntry)>> = Mutex::new(Vec::with_capacity(to_scan_len));

    let blocked_count = existing.blocked.len();
    let scan_one = |model: &DiskModel| {
        let verdict = scan_file(&model.path);
        let (stored_verdict, reason) = match verdict {
            ScanVerdict::Threat(reason) => {
                new_threats.fetch_add(1, Ordering::Relaxed);
                log::warn!("RWGuard threat in {}: {reason}", model.name);
                (StoredVerdict::Threat, Some(reason))
            }
            ScanVerdict::Safe | ScanVerdict::Error => (StoredVerdict::Safe, None),
        };
        let entry = HistoryEntry {
            size: model.size,
            modified_ms: model.modified_ms,
            verdict: stored_verdict,
            reason,
            checked_at: now,
        };
        updates
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((model.name.clone(), entry));

        let done = checked_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(emit) = on_progress {
            let should_emit = {
                let mut guard = last_emit.lock().unwrap_or_else(|p| p.into_inner());
                let elapsed = guard.elapsed();
                if done == to_scan_len || elapsed >= PROGRESS_THROTTLE {
                    *guard = Instant::now();
                    true
                } else {
                    false
                }
            };
            if should_emit {
                let total_threats = threats + new_threats.load(Ordering::Relaxed);
                emit(RwGuardProgress {
                    scanning: done < to_scan_len,
                    total: to_scan_len,
                    checked: done,
                    reused,
                    newly_scanned: done,
                    threats: total_threats,
                    blocked: blocked_count,
                    live_validated: 0,
                    message: Some(format!(
                        "RWGuard: analizando {done} de {to_scan_len} modelos nuevos..."
                    )),
                });
            }
        }
    };

    // Use a bounded thread pool when scanning multiple models; avoid thread
    // creation overhead for tiny incremental updates.
    if to_scan_len < 8 {
        for model in &to_scan {
            scan_one(model);
        }
    } else {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8)
            .min(to_scan_len);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let idx = next_index.fetch_add(1, Ordering::Relaxed);
                    let Some(model) = to_scan.get(idx) else {
                        break;
                    };
                    scan_one(model);
                });
            }
        });
    }

    let final_updates = updates.into_inner().unwrap_or_else(|p| p.into_inner());
    let total_threats = threats + new_threats.load(Ordering::Relaxed);
    history.commit_scan(&key, final_updates, Some(&present_names), now);

    if let Some(emit) = on_progress {
        emit(RwGuardProgress {
            scanning: false,
            total: to_scan_len,
            checked: to_scan_len,
            reused,
            newly_scanned: to_scan_len,
            threats: total_threats,
            blocked: blocked_count,
            live_validated: 0,
            message: None,
        });
    }

    Ok(ScanSummary {
        total,
        reused,
        newly_scanned: to_scan_len,
        threats: total_threats,
    })
}

/// Validates a plain model filename (no directory traversal or path separators).
fn safe_model_file_name(name: &str) -> bool {
    !name.is_empty()
        && is_model_name(name)
        && !name.contains(['/', '\\', ':'])
        && name != "."
        && name != ".."
}

// ---- Tauri app integration ----

pub fn status(app: &AppHandle) -> RwGuardStatus {
    let ctx = app.state::<Ctx>();
    let Ok(dir) = cache_root(app) else {
        return RwGuardStatus {
            total_models: 0,
            validated_models: 0,
            reused_from_history: 0,
            newly_scanned: 0,
            healed_models: 0,
            active_threats: Vec::new(),
            blocked_threats: Vec::new(),
            recent_history: Vec::new(),
            last_scan_at: None,
            scanning: ctx.rwguard.is_scanning(),
        };
    };
    ctx.rwguard.snapshot(&dir)
}

/// Runs an incremental (or full when `force = true`) RWGuard scan over the
/// server cache folder, updating persistent history and emitting UI events.
pub async fn scan_cache(app: &AppHandle, force: bool) -> RwGuardStatus {
    let ctx = app.state::<Ctx>();
    let Ok(dir) = cache_root(app) else {
        return status(app);
    };
    if ctx.rwguard.scanning.swap(true, Ordering::SeqCst) {
        return ctx.rwguard.snapshot(&dir);
    }

    let history = ctx.rwguard.history();
    let dir_clone = dir.clone();
    let event_app = app.clone();
    let started = Instant::now();

    let scan_result = tokio::task::spawn_blocking(move || {
        let on_progress = move |progress: RwGuardProgress| {
            events::emit(&event_app, RWGUARD_PROGRESS, progress);
        };
        scan_dir_incremental(&history, &dir_clone, force, Some(&on_progress))
    })
    .await;

    ctx.rwguard.scanning.store(false, Ordering::SeqCst);

    match scan_result {
        Ok(Ok(summary)) => {
            ctx.rwguard
                .set_last_run(summary.reused, summary.newly_scanned);
            log::info!(
                "RWGuard scan (force={force}): total={} reused={} new={} threats={} in {:?}",
                summary.total,
                summary.reused,
                summary.newly_scanned,
                summary.threats,
                started.elapsed()
            );
        }
        Ok(Err(error)) => {
            log::warn!("RWGuard scan failed for {}: {error}", dir.display());
        }
        Err(error) => {
            log::error!("RWGuard scan task panicked: {error}");
        }
    }

    let snapshot = ctx.rwguard.snapshot(&dir);
    events::emit(app, RWGUARD_STATUS_CHANGED, snapshot.clone());
    snapshot
}

/// Creates a `CacheGuard` wired to emit live progress events during `cache::install`.
pub fn make_cache_guard(app: &AppHandle, dir: &Path) -> Arc<CacheGuard> {
    let ctx = app.state::<Ctx>();
    let event_app = app.clone();
    Arc::new(CacheGuard::new(
        ctx.rwguard.history(),
        dir,
        Arc::new(move |progress: RwGuardProgress| {
            events::emit(&event_app, RWGUARD_PROGRESS, progress);
        }),
    ))
}

/// Publishes the updated `RwGuardStatus` after a cache sync finishes.
pub fn finish_cache_guard(app: &AppHandle, dir: &Path, guard: &CacheGuard) {
    let (reused, newly, live, blocked) = guard.finish();
    let ctx = app.state::<Ctx>();
    ctx.rwguard.set_last_run(reused, newly);
    log::info!(
        "RWGuard sync finished: reused={reused} newly_scanned={newly} live_validated={live} blocked={blocked}"
    );
    let snapshot = ctx.rwguard.snapshot(dir);
    events::emit(app, RWGUARD_STATUS_CHANGED, snapshot);
}

/// Heals one active threat file (or all active threats when `file_name` is
/// `None`) in the server cache folder, re-scans each healed file, and updates
/// the persistent history.
pub async fn heal(app: &AppHandle, file_name: Option<String>) -> RwGuardActionResult {
    let ctx = app.state::<Ctx>();
    let dir = match cache_root(app) {
        Ok(dir) => dir,
        Err(error) => {
            return RwGuardActionResult {
                ok: false,
                affected: 0,
                error: Some(error),
                status: status(app),
            };
        }
    };
    if find_game_process().is_some() {
        return RwGuardActionResult {
            ok: false,
            affected: 0,
            error: Some("Cierra GTA San Andreas antes de reparar archivos del caché.".into()),
            status: ctx.rwguard.snapshot(&dir),
        };
    }
    if ctx.cache.is_syncing() {
        return RwGuardActionResult {
            ok: false,
            affected: 0,
            error: Some("Espera a que termine la sincronización del caché.".into()),
            status: ctx.rwguard.snapshot(&dir),
        };
    }
    if let Some(ref name) = file_name {
        if !safe_model_file_name(name) {
            return RwGuardActionResult {
                ok: false,
                affected: 0,
                error: Some("Nombre de modelo inválido.".into()),
                status: ctx.rwguard.snapshot(&dir),
            };
        }
    }

    let history = ctx.rwguard.history();
    let dir_clone = dir.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let key = folder_key(&dir_clone);
        let snap = history.folder_snapshot(&key);
        let targets: Vec<String> = match file_name {
            Some(name) => vec![name],
            None => snap
                .files
                .iter()
                .filter(|(_, entry)| entry.verdict == StoredVerdict::Threat)
                .map(|(name, _)| name.clone())
                .collect(),
        };

        let mut healed = 0usize;
        let mut updates = Vec::new();
        let now = now_unix_secs();

        for name in targets {
            let path = dir_clone.join(&name);
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                continue;
            }
            match heal_file(&path) {
                Ok(true) => {
                    let meta = fs::symlink_metadata(&path).ok();
                    let size = meta.as_ref().map_or(0, |m| m.len());
                    let modified_ms = modified_millis(meta.and_then(|m| m.modified().ok()));
                    let recheck = scan_file(&path);
                    let (verdict, reason) = match recheck {
                        ScanVerdict::Threat(r) => (StoredVerdict::Threat, Some(r)),
                        ScanVerdict::Safe | ScanVerdict::Error => {
                            healed += 1;
                            (StoredVerdict::Healed, None)
                        }
                    };
                    updates.push((
                        name,
                        HistoryEntry {
                            size,
                            modified_ms,
                            verdict,
                            reason,
                            checked_at: now,
                        },
                    ));
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!("RWGuard heal failed for {}: {error}", path.display());
                }
            }
        }

        if !updates.is_empty() {
            history.commit_scan(&key, updates, None, now);
        }
        healed
    })
    .await
    .unwrap_or(0);

    let snapshot = ctx.rwguard.snapshot(&dir);
    events::emit(app, RWGUARD_STATUS_CHANGED, snapshot.clone());
    RwGuardActionResult {
        ok: true,
        affected: outcome,
        error: None,
        status: snapshot,
    }
}

/// Deletes one threat file (or all threats when `file_name` is `None`) from
/// the server cache folder, and also clears matching blocked-on-download
/// notices.
pub async fn delete_threats(app: &AppHandle, file_name: Option<String>) -> RwGuardActionResult {
    let ctx = app.state::<Ctx>();
    let dir = match cache_root(app) {
        Ok(dir) => dir,
        Err(error) => {
            return RwGuardActionResult {
                ok: false,
                affected: 0,
                error: Some(error),
                status: status(app),
            };
        }
    };
    if find_game_process().is_some() {
        return RwGuardActionResult {
            ok: false,
            affected: 0,
            error: Some("Cierra GTA San Andreas antes de eliminar archivos del caché.".into()),
            status: ctx.rwguard.snapshot(&dir),
        };
    }
    if ctx.cache.is_syncing() {
        return RwGuardActionResult {
            ok: false,
            affected: 0,
            error: Some("Espera a que termine la sincronización del caché.".into()),
            status: ctx.rwguard.snapshot(&dir),
        };
    }
    if let Some(ref name) = file_name {
        if !safe_model_file_name(name) {
            return RwGuardActionResult {
                ok: false,
                affected: 0,
                error: Some("Nombre de modelo inválido.".into()),
                status: ctx.rwguard.snapshot(&dir),
            };
        }
    }

    let history = ctx.rwguard.history();
    let dir_clone = dir.clone();
    let affected = tokio::task::spawn_blocking(move || {
        let key = folder_key(&dir_clone);
        let snap = history.folder_snapshot(&key);
        let targets: Vec<String> = match file_name.as_ref() {
            Some(name) => vec![name.clone()],
            None => snap
                .files
                .iter()
                .filter(|(_, entry)| entry.verdict == StoredVerdict::Threat)
                .map(|(name, _)| name.clone())
                .collect(),
        };

        let mut removed = Vec::new();
        for name in targets {
            let path = dir_clone.join(&name);
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                continue;
            }
            if with_writable(&path, || fs::remove_file(&path)).is_ok() {
                removed.push(name);
            } else {
                log::warn!("RWGuard could not delete {}", path.display());
            }
        }
        history.remove_files(&key, &removed);
        let cleared = history.clear_blocked(&key, file_name.as_deref());
        removed.len() + cleared
    })
    .await
    .unwrap_or(0);

    let snapshot = ctx.rwguard.snapshot(&dir);
    events::emit(app, RWGUARD_STATUS_CHANGED, snapshot.clone());
    RwGuardActionResult {
        ok: true,
        affected,
        error: None,
        status: snapshot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rwguard::scanner::rw;
    use crate::rwguard::scanner::tests::{build_dff, build_txd};

    #[test]
    fn incremental_scan_skips_unchanged_and_detects_modifications() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let hist = RwGuardHistory::open(tmp.path().join(history::FILE_NAME));

        let good_dff = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX);
        let good_txd = build_txd(4, 16, 16, 16, 0x08, rw::D3DFMT_DXT1, 128);
        let bad_dff = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX + 64);

        fs::write(cache_dir.join("0x1.dff"), &good_dff).unwrap();
        fs::write(cache_dir.join("0x2.txd"), &good_txd).unwrap();
        fs::write(cache_dir.join("0x3.dff"), &bad_dff).unwrap();
        fs::write(cache_dir.join("readme.txt"), b"ignored").unwrap();

        // First scan: all 3 models are new.
        let first = scan_dir_incremental(&hist, &cache_dir, false, None).unwrap();
        assert_eq!(
            first,
            ScanSummary {
                total: 3,
                reused: 0,
                newly_scanned: 3,
                threats: 1,
            }
        );

        // Second scan without changes: all 3 reused from history!
        let second = scan_dir_incremental(&hist, &cache_dir, false, None).unwrap();
        assert_eq!(
            second,
            ScanSummary {
                total: 3,
                reused: 3,
                newly_scanned: 0,
                threats: 1,
            }
        );

        // Add one new model and modify 0x3.dff to be clean: only those 2 are scanned.
        fs::write(cache_dir.join("0x4.dff"), &good_dff).unwrap();
        fs::write(cache_dir.join("0x3.dff"), &good_dff).unwrap();
        let third = scan_dir_incremental(&hist, &cache_dir, false, None).unwrap();
        assert_eq!(
            third,
            ScanSummary {
                total: 4,
                reused: 2,
                newly_scanned: 2,
                threats: 0,
            }
        );

        // Forced scan re-analyzes every model.
        let forced = scan_dir_incremental(&hist, &cache_dir, true, None).unwrap();
        assert_eq!(
            forced,
            ScanSummary {
                total: 4,
                reused: 0,
                newly_scanned: 4,
                threats: 0,
            }
        );
    }

    #[test]
    fn cache_guard_blocks_malicious_temp_and_records_safe_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let hist = Arc::new(RwGuardHistory::open(tmp.path().join(history::FILE_NAME)));
        let guard = CacheGuard::new(hist.clone(), &cache_dir, Arc::new(|_| {}));

        let bad_temp = cache_dir.join("0xBAD.dff.1234.sarp-part");
        fs::write(&bad_temp, build_dff(rw::LIGHT, rw::LIGHT_STRUCT_MAX + 32)).unwrap();
        let err = guard
            .validate_temp(&bad_temp, "0xBAD.dff", 100)
            .unwrap_err();
        assert_eq!(err, "Light STRUCT overflow");

        let good_temp = cache_dir.join("0xGOOD.dff.1234.sarp-part");
        let good_bytes = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX);
        fs::write(&good_temp, &good_bytes).unwrap();
        guard
            .validate_temp(&good_temp, "0xGOOD.dff", good_bytes.len() as u64)
            .unwrap();
        let final_path = cache_dir.join("0xGOOD.dff");
        fs::rename(&good_temp, &final_path).unwrap();
        guard.record_landed(&final_path, "0xGOOD.dff", good_bytes.len() as u64);

        let (reused, newly, live, blocked) = guard.finish();
        assert_eq!((reused, newly, live, blocked), (0, 1, 1, 1));

        // Subsequent incremental scan reuses 0xGOOD.dff from history (0 newly scanned).
        let follow = scan_dir_incremental(&hist, &cache_dir, false, None).unwrap();
        assert_eq!(follow.reused, 1);
        assert_eq!(follow.newly_scanned, 0);
    }
}
