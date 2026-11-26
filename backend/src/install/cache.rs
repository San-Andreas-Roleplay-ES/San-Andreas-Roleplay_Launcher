//! Server cache (custom models) under
//! `Documents\GTA San Andreas User Files\SAMP\cache\<server>`, kept in sync
//! with the list our API publishes (`/api/public/launcher/cache`).
//!
//! SA-MP stores every model it downloads as `0x<CRC32>.dff|txd` (IEEE CRC-32
//! of the content, upper-case hex, no padding), and the list uses the same
//! names. A sync:
//!
//! 1. scans the folder: one level, plain files only, links never followed;
//! 2. deletes stale cache files (named like one, not in the list, and not
//!    newer than the last zip upload), but only against a list it fully
//!    trusts: non-empty, dated, every entry valid;
//! 3. when the player is far behind (the missing files weigh more than the
//!    rest of the list's zip), downloads that zip, resumably, and unpacks the
//!    missing files from it (see `package`);
//! 4. downloads whatever is still missing, or has another size, from the
//!    list's `baseUrl`, a few files at a time (fewer on a bad line, see
//!    `pace`), checking size and CRC before the temp file is renamed into
//!    place.
//!
//! It can be cancelled at any point. Re-running it only fetches what is
//! still missing (and resumes the zip), so it resumes for free.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::app::events::{self, CACHE_INSTALL_PROGRESS};
use crate::config::{dev_cache_dir, CACHE_FOLDER, IS_DEV};
use crate::game::launcher::find_game_process;
use crate::install::cancel::Cancel;
use crate::install::fs::{free_space, random_hex};
use crate::install::progress::{percent, Phase, Progress, Reporter, SpeedMeter};
use crate::net::api::{self, Api, BAD_RESPONSE};
use crate::net::http::{
    download_with_resume, next_chunk, send, Failure, Leftover, Limits, ResumeOptions,
};
use crate::rwguard::{self, CacheGuard};
use crate::Ctx;

mod pace;
pub mod package;

use pace::Pace;

/// The health check and the sync that follows it share one list.
const LIST_TTL: Duration = Duration::from_secs(60);
/// For the response: the API's first build lists the whole bucket prefix.
/// The body (a few hundred KB) may then take as long as it keeps arriving.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Each file: waits, never a cap on the whole transfer.
const FILE_LIMITS: Limits = Limits {
    response: Duration::from_secs(30),
    stall: Duration::from_secs(30),
    retry_delay: Duration::from_millis(800),
    max_delay: Duration::from_secs(5),
    attempts: 3,
};
pub(crate) const PROGRESS_THROTTLE: Duration = Duration::from_millis(150);
const WRITE_BUFFER: usize = 64 * 1024;
/// Head room left on the disk after the download.
const SPACE_MARGIN: u64 = 256 * 1024 * 1024;
/// `<name>.<random>.sarp-part`: never a valid cache name, so neither the game
/// nor the sync mistakes a half-written file for a model.
const TEMP_SUFFIX: &str = ".sarp-part";
const PACKAGE_MESSAGE: &str = "Descargando paquete del caché...";

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// How a sync attempt ended. None of it blocks playing: SA-MP downloads any
/// model it lacks from the game server, so the UI only warns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInstallResult {
    /// Every listed file is in place.
    pub ok: bool,
    /// The sync could not run, or stopped as a whole (no list, folder
    /// unusable, no disk space, game open). Per-file failures are not errors:
    /// they are counted in `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Listed files still missing because they could not be downloaded (or
    /// were never tried once the sync gave up or was cancelled).
    pub failed: u64,
    /// Models rejected by RWGuard before landing in the cache folder.
    #[serde(skip_serializing_if = "is_zero")]
    pub blocked: u64,
    /// The user stopped it (`cancel_cache`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl CacheInstallResult {
    fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            failed: 0,
            blocked: 0,
            cancelled: None,
        }
    }

    fn from_run(run: Result<Summary, String>) -> Self {
        match run {
            Ok(summary) => Self {
                ok: summary.missing() == 0 && summary.blocked == 0 && !summary.cancelled,
                error: None,
                failed: summary.missing() as u64,
                blocked: summary.blocked as u64,
                cancelled: summary.cancelled.then_some(true),
            },
            Err(error) => Self::error(error),
        }
    }
}

// ---- names ----

/// The CRC a cache file name promises, for names exactly as the SA-MP client
/// writes them (`^0x[0-9A-F]{1,8}\.(dff|txd)$`); `None` for anything else.
/// Only such names are ever downloaded to or deleted from the folder.
pub fn parse_name(name: &str) -> Option<u32> {
    let (hex, ext) = name.strip_prefix("0x")?.split_once('.')?;
    let hex_ok = (1..=8).contains(&hex.len())
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
    if !hex_ok || !matches!(ext, "dff" | "txd") {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn temp_name(name: &str) -> String {
    format!("{name}.{}{TEMP_SUFFIX}", random_hex(4))
}

/// A temp file left by an interrupted sync.
fn is_temp(name: &str) -> bool {
    name.strip_suffix(TEMP_SUFFIX)
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(target, tag)| {
            parse_name(target).is_some()
                && !tag.is_empty()
                && tag.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

fn is_cache_name(name: &str) -> bool {
    parse_name(name).is_some()
}

// ---- the list ----

/// One file the folder must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    crc: u32,
}

/// The whole cache as one zip on the CDN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipInfo {
    /// With its `?v=` (a new upload, a new URL).
    pub url: String,
    pub bytes: u64,
}

/// The validated list.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheList {
    /// Without a trailing slash: each file is at `{base_url}/{name}`.
    pub base_url: String,
    pub files: Vec<Entry>,
    /// When the server's cache zip was last uploaded. Stale files modified
    /// after it were downloaded by the game and are kept.
    pub zip_uploaded_at: Option<SystemTime>,
    /// That zip, when there is one (and a date for it).
    pub zip: Option<ZipInfo>,
    /// Entries dropped as invalid (bad name, duplicate with another size).
    pub rejected: usize,
}

impl CacheList {
    /// Deleting is only safe against a list we fully trust.
    pub fn may_delete(&self) -> bool {
        !self.files.is_empty() && self.zip_uploaded_at.is_some() && self.rejected == 0
    }

    fn url(&self, entry: &Entry) -> String {
        format!("{}/{}", self.base_url, entry.name)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    base_url: String,
    #[serde(default)]
    zip_uploaded_at: Option<String>,
    /// Checked on its own: a bad one only turns the zip off.
    #[serde(default)]
    zip: Value,
    /// `[name, sizeBytes]` pairs.
    files: Vec<(String, u64)>,
}

/// Validates the API payload. Bad entries are dropped (and turn deletions
/// off); a bad `zip` is ignored; a bad `baseUrl` rejects the whole list.
pub fn parse_list(value: Value, require_https: bool) -> Result<CacheList, String> {
    let payload: Payload = serde_json::from_value(value).map_err(|error| {
        log::warn!("cache list rejected: {error}");
        BAD_RESPONSE.to_string()
    })?;
    let Some(base_url) = check_url(&payload.base_url, require_https, false) else {
        log::warn!("cache list rejected: baseUrl {:?}", payload.base_url);
        return Err(BAD_RESPONSE.into());
    };

    let mut sizes: HashMap<String, u64> = HashMap::with_capacity(payload.files.len());
    let mut files = Vec::with_capacity(payload.files.len());
    let mut rejected = 0;
    for (name, size) in payload.files {
        let Some(crc) = parse_name(&name) else {
            rejected += 1;
            continue;
        };
        match sizes.get(&name) {
            Some(&known) if known == size => {}
            Some(_) => rejected += 1,
            None => {
                sizes.insert(name.clone(), size);
                files.push(Entry { name, size, crc });
            }
        }
    }
    if rejected > 0 {
        log::warn!("cache list: {rejected} invalid entries dropped, deletions disabled");
    }

    let zip_uploaded_at = payload.zip_uploaded_at.as_deref().and_then(|raw| {
        OffsetDateTime::parse(raw, &Rfc3339)
            .map(SystemTime::from)
            .inspect_err(|_| log::warn!("cache list: bad zipUploadedAt {raw:?}"))
            .ok()
    });
    // The API only publishes a zip with its date; without one, no zip.
    let zip = zip_uploaded_at
        .and_then(|_| check_zip(&payload.zip, require_https))
        .filter(|_| !files.is_empty());
    Ok(CacheList {
        base_url,
        files,
        zip_uploaded_at,
        zip,
        rejected,
    })
}

/// `zip: { url, bytes }`, or `None` (absent, null, or unusable).
fn check_zip(value: &Value, require_https: bool) -> Option<ZipInfo> {
    if value.is_null() {
        return None;
    }
    let url = value.get("url").and_then(Value::as_str);
    let bytes = value
        .get("bytes")
        .and_then(Value::as_u64)
        .filter(|&b| b > 0);
    let checked = url.and_then(|url| check_url(url, require_https, true));
    match (checked, bytes) {
        (Some(url), Some(bytes)) => Some(ZipInfo { url, bytes }),
        _ => {
            log::warn!("cache list: zip ignored: {value}");
            None
        }
    }
}

/// `raw` without a trailing slash, if it is a plain http(s) URL we may
/// download from (https only in release builds). A query (the zip's `?v=`)
/// only where `query` allows it.
pub(crate) fn check_url(raw: &str, require_https: bool, query: bool) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let scheme_ok = url.scheme() == "https" || (!require_https && url.scheme() == "http");
    let plain = url.host_str().is_some()
        && (query || url.query().is_none())
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    (scheme_ok && plain).then(|| url.as_str().trim_end_matches('/').to_string())
}

async fn fetch_list(api: &Api) -> Result<CacheList, String> {
    parse_list(api.get(api::CACHE, LIST_TIMEOUT).await?, !IS_DEV)
}

/// The list (cached for a short while), the one-sync-at-a-time flag and the
/// running sync's cancel token.
#[derive(Default)]
pub struct CacheState {
    list: tokio::sync::Mutex<Option<(Instant, Arc<CacheList>)>>,
    syncing: AtomicBool,
    running: Mutex<Option<Cancel>>,
}

impl CacheState {
    /// The list, fetched at most once per `LIST_TTL`. Concurrent callers wait
    /// for the same request. Failures are not cached.
    pub async fn list(&self, api: &Api) -> Result<Arc<CacheList>, String> {
        let mut slot = self.list.lock().await;
        if let Some((at, list)) = slot.as_ref() {
            if at.elapsed() < LIST_TTL {
                return Ok(list.clone());
            }
        }
        let list = Arc::new(fetch_list(api).await?);
        *slot = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    fn begin(&self) -> Option<Busy<'_>> {
        if self.syncing.swap(true, Ordering::SeqCst) {
            return None;
        }
        let cancel = Cancel::default();
        *lock(&self.running) = Some(cancel.clone());
        Some(Busy {
            state: self,
            cancel,
        })
    }

    /// A sync is writing the folder: the game must not start meanwhile.
    pub fn is_syncing(&self) -> bool {
        self.syncing.load(Ordering::SeqCst)
    }

    /// Asks the running sync to stop. `false` when none is running.
    pub fn cancel(&self) -> bool {
        match lock(&self.running).as_ref() {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }
}

struct Busy<'a> {
    state: &'a CacheState,
    cancel: Cancel,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *lock(&self.state.running) = None;
        self.state.syncing.store(false, Ordering::SeqCst);
    }
}

// ---- the folder ----

#[derive(Debug, Clone, PartialEq)]
pub struct LocalFile {
    pub name: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Default)]
pub struct Scan {
    /// Files named like cache files.
    pub files: Vec<LocalFile>,
    /// Leftovers of an interrupted sync.
    pub temps: Vec<String>,
}

/// Lists `dir`, one level deep. Only plain files count: folders, symlinks and
/// junctions are skipped, never followed. A missing folder is an empty scan.
pub fn scan(dir: &Path) -> io::Result<Scan> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(error) => return Err(error),
    };
    let mut scan = Scan::default();
    for entry in entries {
        let entry = entry?;
        // `file_type` does not follow links: a link is never `is_file`.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_cache_name(name) {
            // From the directory listing itself on Windows: no file is opened.
            let meta = entry.metadata()?;
            scan.files.push(LocalFile {
                name: name.to_string(),
                size: meta.len(),
                modified: meta.modified().ok(),
            });
        } else if is_temp(name) {
            scan.temps.push(name.to_string());
        }
    }
    Ok(scan)
}

/// What a sync would do to the folder.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// To download: not in the folder, or there with another size.
    pub missing: Vec<Entry>,
    /// Stale cache files to delete.
    pub stale: Vec<String>,
    /// Cache files not in the list that stay (newer than the last zip, or
    /// deleting is off for this list).
    pub kept: usize,
    /// Listed files already in place.
    pub present: usize,
}

fn bytes_of(entries: &[Entry]) -> u64 {
    entries.iter().map(|e| e.size).sum()
}

/// Deleting more than half the folder's cache files, and more than
/// `WIPE_MIN` of them, is what an incomplete list would do: refused.
const WIPE_MIN: usize = 100;

fn looks_like_a_wipe(stale: usize, local: usize) -> bool {
    stale > WIPE_MIN && stale * 2 > local
}

pub fn plan(list: &CacheList, local: &[LocalFile]) -> Plan {
    let have: HashMap<&str, u64> = local.iter().map(|f| (f.name.as_str(), f.size)).collect();
    let wanted: HashSet<&str> = list.files.iter().map(|e| e.name.as_str()).collect();
    let (present, missing): (Vec<&Entry>, Vec<&Entry>) = list
        .files
        .iter()
        .partition(|e| have.get(e.name.as_str()) == Some(&e.size));

    let cutoff = list.zip_uploaded_at.filter(|_| list.may_delete());
    let mut stale = Vec::new();
    let mut kept = 0;
    for file in local.iter().filter(|f| !wanted.contains(f.name.as_str())) {
        match (cutoff, file.modified) {
            (Some(cutoff), Some(modified)) if modified <= cutoff => stale.push(file.name.clone()),
            _ => kept += 1,
        }
    }
    if looks_like_a_wipe(stale.len(), local.len()) {
        // Even a trusted list can be incomplete: never let one empty a
        // player's cache. Nothing is deleted; downloads still happen.
        log::warn!(
            "cache: {} of {} files would be deleted; the list looks incomplete, deleting none",
            stale.len(),
            local.len()
        );
        kept += stale.len();
        stale.clear();
    }
    Plan {
        missing: missing.into_iter().cloned().collect(),
        stale,
        kept,
        present: present.len(),
    }
}

/// How the missing files arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The list's zip first, then file by file for what it lacked.
    Package,
    FileByFile,
}

/// Free bytes on the disk holding the package and on the one holding the
/// cache (`None`: unknown), and whether they are the same disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub package: Option<u64>,
    pub cache: Option<u64>,
    pub same_disk: bool,
}

/// The package when the list has one, the missing files weigh more than
/// what is left to download of it (so it moves fewer bytes), and it fits
/// next to the unpacked files. Otherwise file by file, which needs less room.
pub fn route(missing_bytes: u64, package_left: Option<u64>, space: Space) -> Route {
    let Some(left) = package_left else {
        return Route::FileByFile;
    };
    if missing_bytes <= left {
        return Route::FileByFile;
    }
    let fits = |free: Option<u64>, need: u64| {
        free.is_none_or(|free| free >= need.saturating_add(SPACE_MARGIN))
    };
    let room = if space.same_disk {
        fits(space.cache, left.saturating_add(missing_bytes))
    } else {
        fits(space.package, left) && fits(space.cache, missing_bytes)
    };
    if room {
        Route::Package
    } else {
        Route::FileByFile
    }
}

/// `C:` for `C:\...`; for other roots the whole prefix. `None` for a
/// relative path.
fn disk_of(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::Prefix(prefix) => Some(match prefix.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                format!("{}:", (d as char).to_ascii_uppercase())
            }
            _ => prefix.as_os_str().to_string_lossy().to_ascii_lowercase(),
        }),
        _ => None,
    }
}

fn probe_space(package_dir: &Path, cache_dir: &Path) -> Space {
    // The package folder may not exist yet: its parent (the temp folder) does.
    let package_disk = package_dir.parent().unwrap_or(package_dir);
    let (a, b) = (disk_of(package_disk), disk_of(cache_dir));
    Space {
        package: free_space(package_disk),
        cache: free_space(cache_dir),
        // Unknown counts as shared: the stricter check.
        same_disk: a.is_none() || a == b,
    }
}

/// Deletes `names` from `dir`. Re-checks each one right before: it must pass
/// `allowed` and still be a plain file (not a link). Returns how many went.
fn remove_files(dir: &Path, names: &[String], allowed: fn(&str) -> bool) -> usize {
    let mut removed = 0;
    for name in names {
        if !allowed(name) {
            log::error!("refusing to delete {name:?} from the cache folder");
            continue;
        }
        let path = dir.join(name);
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file()) {
            continue;
        }
        match with_writable(&path, || fs::remove_file(&path)) {
            Ok(()) => removed += 1,
            Err(error) => log::warn!("could not delete {}: {error}", path.display()),
        }
    }
    removed
}

/// Runs `op`; if Windows refuses because `path` is read-only, clears the
/// attribute and runs it once more.
pub(crate) fn with_writable(path: &Path, op: impl Fn() -> io::Result<()>) -> io::Result<()> {
    match op() {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return Err(error);
            };
            if !meta.file_type().is_file() || !meta.permissions().readonly() {
                return Err(error);
            }
            let mut permissions = meta.permissions();
            // Windows only: this clears FILE_ATTRIBUTE_READONLY.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
            op()
        }
        other => other,
    }
}

/// Fails with a readable message when `needed` bytes (plus a margin) don't
/// fit in `free`. Unknown free space never blocks.
fn check_space(needed: u64, free: Option<u64>) -> Result<(), String> {
    match free {
        Some(free) if free < needed.saturating_add(SPACE_MARGIN) => Err(format!(
            "No hay espacio suficiente en el disco: el caché necesita {} libres y hay {}. \
             Libera espacio y vuelve a intentarlo.",
            gigabytes(needed.saturating_add(SPACE_MARGIN)),
            gigabytes(free)
        )),
        _ => Ok(()),
    }
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

// ---- the sync ----

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Unpacked from the package.
    pub extracted: usize,
    /// Downloaded one by one.
    pub downloaded: usize,
    /// Failed every attempt (or were cut short when the sync gave up).
    pub failed: usize,
    /// Rejected by RWGuard during download or extraction.
    pub blocked: usize,
    /// Never tried: the sync gave up or was cancelled first.
    pub skipped: usize,
    pub deleted: usize,
    pub kept: usize,
    /// The package was used (downloaded, resumed or unpacked).
    pub package: bool,
    pub cancelled: bool,
}

impl Summary {
    /// Listed files this sync could not put in place.
    pub fn missing(&self) -> usize {
        self.failed + self.skipped
    }
}

/// What a sync works with besides the list and the folder.
pub struct Job<'a> {
    /// No redirects: files and package come from the URLs the API gave, and
    /// never with `X-Launcher-Key`.
    pub client: &'a Client,
    pub report: &'a Reporter,
    pub cancel: &'a Cancel,
    /// Where the package is downloaded (`package::dir`).
    pub package_dir: &'a Path,
    pub package_limits: Limits,
    pub file_limits: Limits,
    /// Native RWGuard validator: scans existing cache first, then validates
    /// each model concurrently as it unpacks or downloads.
    pub guard: Option<Arc<CacheGuard>>,
}

impl<'a> Job<'a> {
    pub fn new(
        client: &'a Client,
        report: &'a Reporter,
        cancel: &'a Cancel,
        package_dir: &'a Path,
        guard: Option<Arc<CacheGuard>>,
    ) -> Self {
        Self {
            client,
            report,
            cancel,
            package_dir,
            package_limits: Limits::RESUMABLE,
            file_limits: FILE_LIMITS,
            guard,
        }
    }
}

/// Brings `dir` in line with `list`. Errors are for the sync as a whole
/// (folder unusable, no disk space); per-file failures are counted, and a
/// cancel is reported in the summary.
pub async fn sync(job: &Job<'_>, list: Arc<CacheList>, dir: &Path) -> Result<Summary, String> {
    let report = job.report;
    report(Progress::new(Phase::Preflight, 0.0).message("Comprobando archivos del caché..."));
    tokio::fs::create_dir_all(dir).await.map_err(|error| {
        log::warn!("create {}: {error}", dir.display());
        "No se pudo crear la carpeta del caché.".to_string()
    })?;

    let scanned = scan_clean(dir).await?;
    let plan = plan(&list, &scanned.files);
    let mut summary = Summary {
        kept: plan.kept,
        ..Summary::default()
    };

    if !plan.stale.is_empty() {
        report(
            Progress::new(Phase::Preflight, 0.0).message("Borrando archivos viejos del caché..."),
        );
        let stale_dir = dir.to_path_buf();
        let stale = plan.stale.clone();
        summary.deleted =
            tokio::task::spawn_blocking(move || remove_files(&stale_dir, &stale, is_cache_name))
                .await
                .map_err(|e| e.to_string())?;
        if let Some(guard) = &job.guard {
            guard.remove_stale(&plan.stale);
        }
    }

    // Step 1: validate existing models in the server cache first (reusing
    // persistent history so unchanged models finish in milliseconds).
    if let Some(guard) = &job.guard {
        if !scanned.files.is_empty() && !job.cancel.is_cancelled() {
            report(
                Progress::new(Phase::Verify, 0.0)
                    .message("Validando caché existente con RWGuard..."),
            );
            let existing_dir = dir.to_path_buf();
            let guard_ref = guard.clone();
            if let Err(error) =
                tokio::task::spawn_blocking(move || guard_ref.scan_existing(&existing_dir))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
            {
                log::warn!("RWGuard pre-sync scan failed: {error}");
            }
        }
    }

    let mut missing = plan.missing;
    if missing.is_empty() {
        package::prune(job.package_dir, None);
        return Ok(summary);
    }

    let mut unpacked = false;
    if let Some(zip) = &list.zip {
        let left = zip.bytes - package::on_disk(job.package_dir, zip);
        let space = probe_space(job.package_dir, dir);
        let route = route(bytes_of(&missing), Some(left), space);
        log::info!(
            "cache: {} files ({} bytes) missing, package {} bytes ({left} left), {space:?}: {route:?}",
            missing.len(),
            bytes_of(&missing),
            zip.bytes
        );
        if route == Route::Package {
            summary.package = true;
            match unpack(job, zip, dir, &missing).await {
                Unpack::Cancelled(written, blocked) => {
                    summary.extracted = written;
                    summary.blocked += blocked;
                    summary.skipped = missing.len().saturating_sub(written + blocked);
                    summary.cancelled = true;
                    return Ok(summary);
                }
                Unpack::Done(written, blocked) => {
                    summary.extracted = written;
                    summary.blocked += blocked;
                    unpacked = true;
                    missing = plan_again(&list, dir).await?;
                    if let Some(guard) = &job.guard {
                        // Do not re-download a file that RWGuard just rejected
                        // from the package.
                        missing.retain(|entry| !guard.was_blocked(&entry.name));
                    }
                }
                // Never worse off: the same files, one by one.
                Unpack::Failed => {}
            }
        }
    } else {
        // No zip in the list: whatever we kept of an older one is useless.
        package::prune(job.package_dir, None);
    }

    if missing.is_empty() {
        package::prune(job.package_dir, None);
        return Ok(summary);
    }
    check_space(bytes_of(&missing), free_space(dir))?;

    let total = missing.len();
    let (downloaded, failed, blocked) = download(job, &list, dir, missing, unpacked).await;
    summary.downloaded = downloaded;
    summary.failed = failed;
    summary.blocked += blocked;
    summary.skipped = total.saturating_sub(downloaded + failed + blocked);
    // A cancel that came after the last file is no cancel.
    summary.cancelled = job.cancel.is_cancelled() && summary.missing() > 0;
    if summary.missing() == 0 && summary.blocked == 0 {
        package::prune(job.package_dir, None);
    }
    Ok(summary)
}

/// Scans `dir` and removes the temp files of an interrupted sync.
async fn scan_clean(dir: &Path) -> Result<Scan, String> {
    let scan_dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let scan = scan(&scan_dir)?;
        remove_files(&scan_dir, &scan.temps, is_temp);
        Ok::<_, io::Error>(scan)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|error| {
        log::warn!("scan {}: {error}", dir.display());
        "No se pudo leer la carpeta del caché.".to_string()
    })
}

/// What is still missing, from the folder as it is now.
async fn plan_again(list: &CacheList, dir: &Path) -> Result<Vec<Entry>, String> {
    let scanned = scan_clean(dir).await?;
    Ok(plan(list, &scanned.files).missing)
}

enum Unpack {
    /// The files written (and blocked by RWGuard) before the cancel. The package stays.
    Cancelled(usize, usize),
    Done(usize, usize),
    /// Download given up, or the zip is unreadable: go file by file.
    Failed,
}

/// Downloads (or resumes) the package and unpacks the missing files.
async fn unpack(job: &Job<'_>, zip: &ZipInfo, dir: &Path, missing: &[Entry]) -> Unpack {
    let name = package::file_name(zip);
    // Older uploads (and anything else) go; this one's partial stays.
    package::prune(job.package_dir, Some(&name));
    if let Err(error) = tokio::fs::create_dir_all(job.package_dir).await {
        log::warn!("create {}: {error}", job.package_dir.display());
        return Unpack::Failed;
    }
    let dest = job.package_dir.join(&name);
    let opts = ResumeOptions {
        url: &zip.url,
        dest: &dest,
        expected_size: zip.bytes,
        // The name is tied to this upload; each entry is checked on unpack.
        leftover: Leftover::Trusted,
        limits: job.package_limits,
        message: Some(PACKAGE_MESSAGE),
    };
    if let Err(error) = download_with_resume(job.client, &opts, job.report, job.cancel).await {
        if job.cancel.is_cancelled() {
            log::info!("cache package download cancelled; the partial stays");
            return Unpack::Cancelled(0, 0);
        }
        log::warn!("cache package download failed ({error}); going file by file");
        return Unpack::Failed;
    }

    (job.report)(Progress::new(Phase::Extract, 0.0).message("Abriendo el paquete del caché..."));
    let (zip_path, target, wanted) = (dest.clone(), dir.to_path_buf(), missing.to_vec());
    let (report, cancel, guard) = (job.report.clone(), job.cancel.clone(), job.guard.clone());
    let started = Instant::now();
    let unpacked = tokio::task::spawn_blocking(move || {
        package::extract(
            &zip_path,
            &target,
            &wanted,
            &report,
            &cancel,
            guard.as_deref(),
        )
    })
    .await;
    match unpacked {
        Ok(Ok(unpacked)) if job.cancel.is_cancelled() => {
            log::info!("cache package: unpack cancelled after {}", unpacked.written);
            Unpack::Cancelled(unpacked.written, unpacked.blocked)
        }
        Ok(Ok(unpacked)) => {
            log::info!(
                "cache package: {unpacked:?} of {} missing in {:?}",
                missing.len(),
                started.elapsed()
            );
            package::prune(job.package_dir, None);
            Unpack::Done(unpacked.written, unpacked.blocked)
        }
        Ok(Err(error)) => {
            log::warn!("cache package unreadable ({error}); deleting it, going file by file");
            package::prune(job.package_dir, None);
            Unpack::Failed
        }
        Err(error) => {
            log::error!("cache package unpack panicked: {error}");
            Unpack::Failed
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Download progress across all the files in flight.
struct Tracker {
    report: Reporter,
    files: usize,
    bytes: u64,
    files_done: usize,
    bytes_done: u64,
    /// After the package: these are the files it lacked.
    remaining: bool,
    speed: SpeedMeter,
    last_emit: Option<Instant>,
}

impl Tracker {
    fn add(&mut self, bytes: u64) {
        self.bytes_done += bytes;
        let now = Instant::now();
        self.speed.push(now, bytes);
        self.emit(now, false);
    }

    /// A failed attempt's bytes no longer count.
    fn rollback(&mut self, bytes: u64) {
        self.bytes_done = self.bytes_done.saturating_sub(bytes);
    }

    fn finished(&mut self) {
        self.files_done += 1;
        self.emit(Instant::now(), self.files_done == self.files);
    }

    fn emit(&mut self, now: Instant, force: bool) {
        if !force
            && self
                .last_emit
                .is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE)
        {
            return;
        }
        self.last_emit = Some(now);
        let message = if self.remaining {
            format!(
                "Descargando {} de {} archivos restantes...",
                self.files_done, self.files
            )
        } else {
            format!(
                "Descargando {} de {} archivos...",
                self.files_done, self.files
            )
        };
        (self.report)(Progress {
            speed_bps: Some(self.speed.bytes_per_second(now)),
            ..Progress::new(Phase::Download, percent(self.bytes_done, self.bytes))
                .bytes(self.bytes_done, self.bytes)
                .message(message)
        });
    }
}

enum FileErr {
    Http(Failure),
    Blocked(String),
}

/// Downloads `files`, as many at once as the line takes (`Pace`). Retries go
/// back to the front of the queue, so they wait for a free slot like
/// everything else: when the pace drops, the files already failing slow down
/// too. Returns `(downloaded, failed, blocked)`.
async fn download(
    job: &Job<'_>,
    list: &CacheList,
    dir: &Path,
    files: Vec<Entry>,
    remaining: bool,
) -> (usize, usize, usize) {
    let tracker = Mutex::new(Tracker {
        report: job.report.clone(),
        files: files.len(),
        bytes: bytes_of(&files),
        files_done: 0,
        bytes_done: 0,
        remaining,
        speed: SpeedMeter::default(),
        last_emit: None,
    });
    lock(&tracker).emit(Instant::now(), true);
    let pace = Mutex::new(Pace::default());
    let mut queue: VecDeque<(Entry, u32)> = files.into_iter().map(|e| (e, 0)).collect();
    let mut running = FuturesUnordered::new();
    let (mut downloaded, mut failed, mut blocked, mut streak) = (0, 0, 0, 0);
    let mut gave_up = false;
    loop {
        if !gave_up && !job.cancel.is_cancelled() {
            while running.len() < lock(&pace).limit() {
                let Some((entry, tries)) = queue.pop_front() else {
                    break;
                };
                running.push(attempt(job, list, dir, entry, tries, &tracker, &pace));
            }
        }
        let Some((entry, tries, result)) = running.next().await else {
            break;
        };
        match result {
            Ok(()) => {
                downloaded += 1;
                streak = 0;
                lock(&tracker).finished();
            }
            Err(FileErr::Blocked(reason)) => {
                log::warn!("cache file {} blocked by RWGuard: {reason}", entry.name);
                blocked += 1;
                streak = 0;
                lock(&tracker).finished();
            }
            Err(FileErr::Http(Failure::Cancelled)) => {}
            Err(FileErr::Http(failure)) if tries + 1 < job.file_limits.attempts && !gave_up => {
                log::debug!(
                    "cache file {} attempt {}: {failure:?}",
                    entry.name,
                    tries + 1
                );
                queue.push_front((entry, tries + 1));
            }
            Err(FileErr::Http(failure)) => {
                log::warn!("cache file {} failed: {failure:?}", entry.name);
                failed += 1;
                streak += 1;
                if !gave_up && lock(&pace).give_up(streak) {
                    log::warn!("cache sync: {streak} files failed in a row, giving up");
                    gave_up = true;
                }
            }
        }
    }
    (downloaded, failed, blocked)
}

/// One attempt at one file (`tries` before it). Timeouts and stalls slow the
/// pace down; a success counts towards speeding it up.
async fn attempt(
    job: &Job<'_>,
    list: &CacheList,
    dir: &Path,
    entry: Entry,
    tries: u32,
    tracker: &Mutex<Tracker>,
    pace: &Mutex<Pace>,
) -> (Entry, u32, Result<(), FileErr>) {
    if tries > 0 && !job.cancel.sleep(job.file_limits.delay(tries - 1)).await {
        return (entry, tries, Err(FileErr::Http(Failure::Cancelled)));
    }
    let epoch = lock(pace).epoch();
    let result = fetch_once(job, &list.url(&entry), dir, &entry, tracker).await;
    match &result {
        Ok(()) | Err(FileErr::Blocked(_)) => lock(pace).success(),
        Err(FileErr::Http(Failure::Slow(_))) => lock(pace).slow(epoch),
        Err(FileErr::Http(_)) => {}
    }
    (entry, tries, result)
}

/// Downloads to a temp name next to the target, validates concurrently with
/// RWGuard on the blocking pool, and renames into place once size, CRC and
/// RenderWare structure check out. Nothing is left behind on failure, block or
/// cancel.
async fn fetch_once(
    job: &Job<'_>,
    url: &str,
    dir: &Path,
    entry: &Entry,
    tracker: &Mutex<Tracker>,
) -> Result<(), FileErr> {
    let temp = dir.join(temp_name(&entry.name));
    let target = dir.join(&entry.name);
    let mut counted = 0u64;
    let result = match transfer(job, url, &temp, entry, |n| {
        counted += n;
        lock(tracker).add(n);
    })
    .await
    {
        Ok(()) => {
            // Off the download task: RWGuard validation + rename run on the
            // blocking pool in parallel with the other downloads in flight.
            let (from, to) = (temp.clone(), target);
            let name = entry.name.clone();
            let size = entry.size;
            let guard = job.guard.clone();
            match tokio::task::spawn_blocking(move || {
                if let Some(ref g) = guard {
                    g.validate_temp(&from, &name, size)
                        .map_err(FileErr::Blocked)?;
                }
                with_writable(&to, || fs::rename(&from, &to))
                    .map_err(|error| FileErr::Http(Failure::Fatal(format!("rename: {error}"))))?;
                if let Some(ref g) = guard {
                    g.record_landed(&to, &name, size);
                }
                Ok(())
            })
            .await
            {
                Ok(res) => res,
                Err(e) => Err(FileErr::Http(Failure::Fatal(e.to_string()))),
            }
        }
        Err(error) => Err(FileErr::Http(error)),
    };
    if let Err(ref err) = result {
        let _ = tokio::fs::remove_file(&temp).await;
        if !matches!(err, FileErr::Blocked(_)) {
            lock(tracker).rollback(counted);
        }
    }
    result
}

async fn transfer(
    job: &Job<'_>,
    url: &str,
    temp: &Path,
    entry: &Entry,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), Failure> {
    let limits = &job.file_limits;
    let response = send(job.client.get(url), limits.response, job.cancel).await?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(Failure::status(status));
    }
    if let Some(length) = response.content_length().filter(|&l| l != entry.size) {
        return Err(Failure::Fatal(format!(
            "Content-Length {length}, listed {}",
            entry.size
        )));
    }

    let file = tokio::fs::File::create(temp)
        .await
        .map_err(|e| Failure::Fatal(format!("create: {e}")))?;
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut crc = crc32fast::Hasher::new();
    let mut received = 0u64;
    let mut body = response.bytes_stream();
    let written = async {
        while let Some(chunk) = next_chunk(&mut body, limits.stall, job.cancel).await? {
            received += chunk.len() as u64;
            if received > entry.size {
                return Err(Failure::Fatal(format!(
                    "more than the listed {} bytes",
                    entry.size
                )));
            }
            crc.update(&chunk);
            writer
                .write_all(&chunk)
                .await
                .map_err(|e| Failure::Fatal(format!("write: {e}")))?;
            on_bytes(chunk.len() as u64);
        }
        writer
            .flush()
            .await
            .map_err(|e| Failure::Fatal(format!("write: {e}")))
    }
    .await;
    // Close the handle before anyone renames or deletes the temp file, on the
    // blocking pool: closing a new file is where real-time antivirus scans
    // it, and every download in flight shares this task.
    let file = writer.into_inner().into_std().await;
    let _ = tokio::task::spawn_blocking(move || drop(file)).await;
    written?;

    if received != entry.size {
        // The connection closed early.
        return Err(Failure::Slow(format!("{received} of {} bytes", entry.size)));
    }
    let actual = crc.finalize();
    if actual != entry.crc {
        return Err(Failure::Fatal(format!(
            "content CRC {actual:X} does not match the name"
        )));
    }
    Ok(())
}

// ---- the app side ----

pub fn cache_root(app: &AppHandle) -> Result<PathBuf, String> {
    // Debug builds only (release ignores it): keeps QA off the real folder.
    if let Some(dir) = dev_cache_dir() {
        return Ok(dir);
    }
    let documents = app
        .path()
        .document_dir()
        .map_err(|_| "No se encontró la carpeta Documentos.".to_string())?;
    Ok(documents
        .join("GTA San Andreas User Files")
        .join("SAMP")
        .join("cache")
        .join(CACHE_FOLDER))
}

/// "Faltan N archivos del caché; ...": the files a sync left behind, which the
/// game fetches by itself.
pub fn missing_message(missing: usize) -> String {
    if missing == 1 {
        "Falta 1 archivo del caché; el juego lo descargará al entrar.".into()
    } else {
        format!("Faltan {missing} archivos del caché; el juego los descargará al entrar.")
    }
}

pub async fn install(app: &AppHandle) -> CacheInstallResult {
    let ctx = app.state::<Ctx>();
    let Some(busy) = ctx.cache.begin() else {
        return CacheInstallResult::error("Ya se está actualizando el caché.");
    };
    let event_app = app.clone();
    let report: Reporter =
        Arc::new(move |p: Progress| events::emit(&event_app, CACHE_INSTALL_PROGRESS, p));
    let result = CacheInstallResult::from_run(run(app, &report, &busy.cancel).await);
    match (
        &result.error,
        result.cancelled,
        result.failed,
        result.blocked,
    ) {
        (None, None, 0, 0) => report(Progress::new(Phase::Done, 100.0)),
        (None, None, 0, blocked) => {
            log::warn!("cache sync: RWGuard blocked {blocked} unsafe model(s)");
            let message = if blocked == 1 {
                "RWGuard bloqueó 1 modelo inseguro durante la descarga.".to_string()
            } else {
                format!("RWGuard bloqueó {blocked} modelos inseguros durante la descarga.")
            };
            report(Progress::new(Phase::Error, 0.0).message(message));
        }
        (None, Some(_), failed, _) => {
            log::info!("cache sync cancelled, {failed} files left for the game");
            // Not an error: the row goes straight to the re-check.
            report(
                Progress::new(Phase::Preflight, 0.0).message("Comprobando archivos del caché..."),
            );
        }
        (None, None, failed, _) => {
            log::warn!("cache sync: {failed} files could not be downloaded");
            let message = missing_message(failed as usize);
            report(Progress::new(Phase::Error, 0.0).message(message));
        }
        (Some(error), _, _, _) => {
            log::warn!("cache sync failed: {error}");
            report(Progress::new(Phase::Error, 0.0).message(error.clone()));
        }
    }
    result
}

/// Stops the running sync, if any (`false`: there was none). It ends within
/// a moment: files in flight are dropped, the package partial stays for the
/// next run.
pub fn cancel(app: &AppHandle) -> bool {
    let cancelled = app.state::<Ctx>().cache.cancel();
    log::info!("cache sync cancel requested (running: {cancelled})");
    cancelled
}

async fn run(app: &AppHandle, report: &Reporter, cancel: &Cancel) -> Result<Summary, String> {
    report(Progress::new(Phase::Preflight, 0.0).message("Preparando caché..."));
    // The game reads (and writes) this folder: never pull it from under it.
    if find_game_process().is_some() {
        return Err("Cierra GTA San Andreas antes de actualizar el caché.".into());
    }
    let ctx = app.state::<Ctx>();
    let Some(list) = cancel.run(ctx.cache.list(&ctx.api)).await else {
        return Ok(Summary {
            cancelled: true,
            ..Summary::default()
        });
    };
    let list = list.map_err(|error| {
        log::warn!("cache list: {error}");
        "No se pudo obtener la lista del caché. Revisa tu conexión a Internet.".to_string()
    })?;
    if list.files.is_empty() {
        // Nothing published yet: there is nothing to fetch and nothing may be
        // deleted.
        log::info!("cache list is empty, nothing to sync");
        return Ok(Summary::default());
    }
    let dir = cache_root(app)?;
    let package_dir = package::dir(&std::env::temp_dir());
    log::info!(
        "cache sync into {} ({} files listed, package {:?})",
        dir.display(),
        list.files.len(),
        list.zip.as_ref().map(|z| z.bytes)
    );

    let started = Instant::now();
    let guard = rwguard::make_cache_guard(app, &dir);
    let job = Job::new(
        ctx.api.client(),
        report,
        cancel,
        &package_dir,
        Some(guard.clone()),
    );
    let sync_result = sync(&job, list, &dir).await;
    rwguard::finish_cache_guard(app, &dir, &guard);
    let summary = sync_result?;
    log::info!("cache sync: {summary:?} in {:?}", started.elapsed());
    if summary.skipped > 0 && !summary.cancelled {
        log::warn!("cache sync gave up: the connection seems to be gone");
    }
    // Files that failed are not an error: the game downloads them itself.
    Ok(summary)
}

#[cfg(test)]
mod sync_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::time::UNIX_EPOCH;

    pub fn entry(name: &str, size: u64) -> Entry {
        Entry {
            name: name.into(),
            size,
            crc: parse_name(name).unwrap(),
        }
    }

    pub fn list_of(files: Vec<Entry>, zip_uploaded_at: Option<SystemTime>) -> CacheList {
        CacheList {
            base_url: "https://cdn.example/cache-files/x".into(),
            files,
            zip_uploaded_at,
            zip: None,
            rejected: 0,
        }
    }

    fn local(name: &str, size: u64, modified: Option<SystemTime>) -> LocalFile {
        LocalFile {
            name: name.into(),
            size,
            modified,
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn crc32fast_is_plain_ieee_crc32() {
        // The standard check value; the API's table is the same IEEE one.
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn validates_names_like_the_client_writes_them() {
        assert_eq!(parse_name("0x1A2B3C4D.dff"), Some(0x1A2B_3C4D));
        assert_eq!(parse_name("0xF.txd"), Some(0xF));
        assert_eq!(parse_name("0x0.dff"), Some(0));
        for bad in [
            "0x1a2b.dff",      // lower-case hex
            "0x1A2B.DFF",      // upper-case extension
            "0X1A2B.dff",      // upper-case prefix
            "0x.dff",          // no digits
            "0x123456789.dff", // more than 8 digits
            "0x1A2G.dff",      // not hex
            "0x1A2B.col",      // other extension
            "0x1A2B.dff.bak",
            "0x1A2B",
            "1A2B.dff",
            "..\\0x1A2B.dff",
            "../0x1A2B.dff",
            "sub/0x1A2B.dff",
            "0x1A2B.dff:stream",
            " 0x1A2B.dff",
            "samp.saa",
            "",
        ] {
            assert_eq!(parse_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn recognises_only_our_temp_files() {
        let temp = temp_name("0xABC.dff");
        assert!(temp.starts_with("0xABC.dff.") && temp.ends_with(".sarp-part"));
        assert!(is_temp(&temp));
        assert!(!is_cache_name(&temp));
        assert!(!is_temp("0xABC.dff"));
        assert!(!is_temp("0xABC.dff.sarp-part"));
        assert!(!is_temp("notes.txt.0011.sarp-part"));
        assert!(!is_temp("0xABC.dff.zz.sarp-part"));
    }

    #[test]
    fn parses_and_validates_the_list() {
        let value = json!({
            "baseUrl": "https://sarp-public.b-cdn.net/cache-files/135.148.128.72.7777/",
            "zipUploadedAt": "2026-11-22T23:50:57.000Z",
            "count": 6,
            "totalBytes": 0,
            "files": [
                ["0xA.dff", 10], ["0xB.txd", 20],
                ["0xA.dff", 10],          // duplicate, same size: fine
                ["0xb.txd", 20],          // bad name
                ["../0xC.dff", 1],        // bad name
                ["0xB.txd", 21]           // duplicate with another size
            ]
        });
        let list = parse_list(value, true).unwrap();
        assert_eq!(
            list.base_url,
            "https://sarp-public.b-cdn.net/cache-files/135.148.128.72.7777"
        );
        assert_eq!(list.files, vec![entry("0xA.dff", 10), entry("0xB.txd", 20)]);
        assert_eq!(list.rejected, 3);
        assert!(!list.may_delete(), "a list with bad entries never deletes");
        assert_eq!(
            list.zip_uploaded_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_795_391_457))
        );
        assert_eq!(list.zip, None, "an API without zip");
        assert_eq!(
            list.url(&list.files[0]),
            format!("{}/0xA.dff", list.base_url)
        );
    }

    #[test]
    fn parses_the_zip_and_ignores_a_bad_one() {
        let with_zip = |zip: Value, date: Value| {
            json!({
                "baseUrl": "https://sarp-public.b-cdn.net/cache-files/x",
                "zipUploadedAt": date,
                "files": [["0xA.dff", 10]],
                "zip": zip
            })
        };
        let date = json!("2026-11-22T23:50:57.000Z");
        let good = json!({
            "url": "https://sarp-public.b-cdn.net/cache/135.148.128.72.7777.zip?v=abc123",
            "bytes": 850_396_987u64
        });
        let list = parse_list(with_zip(good.clone(), date.clone()), true).unwrap();
        assert_eq!(
            list.zip,
            Some(ZipInfo {
                url: "https://sarp-public.b-cdn.net/cache/135.148.128.72.7777.zip?v=abc123".into(),
                bytes: 850_396_987
            })
        );
        assert!(list.may_delete());

        // Null, or no date for it: no zip. The rest of the list still counts.
        for (zip, date) in [
            (Value::Null, date.clone()),
            (good.clone(), Value::Null),
            (good.clone(), json!("not a date")),
        ] {
            let list = parse_list(with_zip(zip, date), true).unwrap();
            assert_eq!(list.zip, None);
            assert_eq!(list.files.len(), 1);
        }
        // Unusable zips are dropped, never the list.
        for bad in [
            json!({ "url": "http://cdn.example/x.zip", "bytes": 5 }),
            json!({ "url": "https://user:pw@cdn.example/x.zip", "bytes": 5 }),
            json!({ "url": "https://cdn.example/x.zip#frag", "bytes": 5 }),
            json!({ "url": "file:///C:/x.zip", "bytes": 5 }),
            json!({ "url": "https://cdn.example/x.zip", "bytes": 0 }),
            json!({ "url": "https://cdn.example/x.zip", "bytes": "5" }),
            json!({ "url": "https://cdn.example/x.zip" }),
            json!({ "bytes": 5 }),
            json!("https://cdn.example/x.zip"),
            json!(42),
        ] {
            let list = parse_list(with_zip(bad.clone(), date.clone()), true).unwrap();
            assert_eq!(list.zip, None, "{bad}");
        }
        // http is fine for a debug build talking to a local mock.
        let local = json!({ "url": "http://127.0.0.1:9/cache.zip?v=1", "bytes": 5 });
        assert!(parse_list(with_zip(local, date.clone()), false)
            .unwrap()
            .zip
            .is_some());
        // An empty list has nothing to unpack.
        let empty = json!({
            "baseUrl": "https://x", "zipUploadedAt": date, "files": [], "zip": good
        });
        assert_eq!(parse_list(empty, true).unwrap().zip, None);
    }

    #[test]
    fn parses_the_empty_list_the_api_answers_today() {
        let body = br#"{"message":"OK","httpCode":200,"source":"launcher","payload":{"baseUrl":"https://sarp-public.b-cdn.net/cache-files/135.148.128.72.7777","zipUploadedAt":"2026-11-22T23:50:57.000Z","count":0,"totalBytes":0,"files":[]}}"#;
        let list = parse_list(api::payload(body).unwrap(), true).unwrap();
        assert!(list.files.is_empty());
        assert!(!list.may_delete());
        assert_eq!(list.zip, None);
    }

    #[test]
    fn rejects_unusable_base_urls() {
        let with_base = |base: &str| json!({ "baseUrl": base, "zipUploadedAt": null, "files": [] });
        for bad in [
            "http://cdn.example/x",
            "ftp://cdn.example/x",
            "https://cdn.example/x?sig=1",
            "https://user:pw@cdn.example/x",
            "file:///C:/Windows",
            "not a url",
        ] {
            assert_eq!(
                parse_list(with_base(bad), true).unwrap_err(),
                BAD_RESPONSE,
                "{bad}"
            );
        }
        // Plain http is fine for a debug build talking to a local mock.
        assert!(parse_list(with_base("http://127.0.0.1:9/files"), false).is_ok());
        // Wrong shapes.
        assert!(parse_list(json!({ "baseUrl": "https://x", "files": {} }), true).is_err());
        assert!(parse_list(
            json!({ "baseUrl": "https://x", "files": [["0xA.dff", "10"]] }),
            true
        )
        .is_err());
        assert!(parse_list(json!([]), true).is_err());
    }

    #[test]
    fn bad_or_missing_dates_turn_deletions_off() {
        let base = |date: Value| json!({ "baseUrl": "https://x", "zipUploadedAt": date, "files": [["0xA.dff", 1]] });
        assert!(parse_list(base(json!("2026-11-22T23:50:57Z")), true)
            .unwrap()
            .may_delete());
        for date in [Value::Null, json!("yesterday"), json!(5)] {
            let list = parse_list(base(date.clone()), true);
            // A non-string date is a shape error; a bad string just disables deletes.
            if let Ok(list) = list {
                assert_eq!(list.zip_uploaded_at, None, "{date}");
                assert!(!list.may_delete());
            }
        }
        let missing = json!({ "baseUrl": "https://x", "files": [["0xA.dff", 1]] });
        assert!(!parse_list(missing, true).unwrap().may_delete());
    }

    #[test]
    fn plans_missing_files_and_wrong_sizes() {
        let list = list_of(
            vec![
                entry("0xA.dff", 10),
                entry("0xB.txd", 20),
                entry("0xC.dff", 30),
            ],
            Some(at(1000)),
        );
        let plan = plan(
            &list,
            &[
                local("0xA.dff", 10, Some(at(1))),
                local("0xB.txd", 19, Some(at(1))),
            ],
        );
        assert_eq!(
            plan.missing,
            vec![entry("0xB.txd", 20), entry("0xC.dff", 30)]
        );
        assert_eq!(bytes_of(&plan.missing), 50);
        assert_eq!(plan.present, 1);
        assert!(plan.stale.is_empty());
    }

    #[test]
    fn the_package_only_when_it_moves_fewer_bytes_and_fits() {
        const MB: u64 = 1024 * 1024;
        const GB: u64 = 1024 * MB;
        let plenty = Space {
            package: Some(100 * GB),
            cache: Some(100 * GB),
            same_disk: true,
        };
        // No zip in the list (old API, or none published): file by file.
        assert_eq!(route(1800 * MB, None, plenty), Route::FileByFile);
        // A fresh folder: 1.8 GB missing against an 850 MB zip.
        assert_eq!(route(1800 * MB, Some(850 * MB), plenty), Route::Package);
        // Nearly complete: a few skins.
        assert_eq!(route(40 * MB, Some(850 * MB), plenty), Route::FileByFile);
        // Exactly the zip's weight: no gain, file by file.
        assert_eq!(route(850 * MB, Some(850 * MB), plenty), Route::FileByFile);
        // What counts is what is left of the zip: 700 MB missing, 600 MB of
        // the zip already downloaded (250 MB left).
        assert_eq!(route(700 * MB, Some(250 * MB), plenty), Route::Package);
        // A finished zip from a cancelled unpack: nothing left to download.
        assert_eq!(route(1, Some(0), plenty), Route::Package);

        // Same disk: needs zip + files + margin.
        let tight = |free: u64| Space {
            package: Some(free),
            cache: Some(free),
            same_disk: true,
        };
        let need = 850 * MB + 1800 * MB + SPACE_MARGIN;
        assert_eq!(
            route(1800 * MB, Some(850 * MB), tight(need)),
            Route::Package
        );
        assert_eq!(
            route(1800 * MB, Some(850 * MB), tight(need - 1)),
            Route::FileByFile
        );
        // Different disks: each needs its own part.
        let split = |package: u64, cache: u64| Space {
            package: Some(package),
            cache: Some(cache),
            same_disk: false,
        };
        let (zip, files) = (850 * MB + SPACE_MARGIN, 1800 * MB + SPACE_MARGIN);
        assert_eq!(
            route(1800 * MB, Some(850 * MB), split(zip, files)),
            Route::Package
        );
        assert_eq!(
            route(1800 * MB, Some(850 * MB), split(zip - 1, files)),
            Route::FileByFile
        );
        assert_eq!(
            route(1800 * MB, Some(850 * MB), split(zip, files - 1)),
            Route::FileByFile
        );
        // Unknown free space never blocks.
        let unknown = Space {
            package: None,
            cache: None,
            same_disk: true,
        };
        assert_eq!(route(1800 * MB, Some(850 * MB), unknown), Route::Package);
    }

    #[test]
    fn tells_disks_apart() {
        assert_eq!(
            disk_of(Path::new(r"C:\Users\x\AppData\Local\Temp")).as_deref(),
            Some("C:")
        );
        assert_eq!(disk_of(Path::new(r"c:\tmp")).as_deref(), Some("C:"));
        assert_eq!(disk_of(Path::new(r"\\?\D:\Games")).as_deref(), Some("D:"));
        assert_eq!(disk_of(Path::new("relative")), None);
        let dir = tempfile::tempdir().unwrap();
        let space = probe_space(&dir.path().join("sarp-cache-zip"), dir.path());
        assert!(space.same_disk);
        assert!(space.package.is_some() && space.cache.is_some());
    }

    #[test]
    fn stale_files_go_unless_newer_than_the_zip() {
        let list = list_of(vec![entry("0xA.dff", 10)], Some(at(1000)));
        let plan = plan(
            &list,
            &[
                local("0xA.dff", 10, Some(at(1))),
                local("0xD1.dff", 5, Some(at(999))), // older: stale
                local("0xD2.txd", 5, Some(at(1000))), // same second: stale
                local("0xD3.dff", 5, Some(at(1001))), // game download after the zip
                local("0xD4.dff", 5, None),          // unknown time: keep
            ],
        );
        assert_eq!(plan.stale, vec!["0xD1.dff", "0xD2.txd"]);
        assert_eq!(plan.kept, 2);
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn never_deletes_most_of_a_big_folder() {
        let list = list_of(vec![entry("0xA.dff", 1)], Some(at(1000)));
        let old = |n: usize| -> Vec<LocalFile> {
            (0..n)
                .map(|i| local(&format!("0x{:X}.dff", 0x100 + i), 1, Some(at(1))))
                .collect()
        };
        // Files the game got after the zip: kept, but they count as the folder.
        let with_fresh = |mut files: Vec<LocalFile>, fresh: usize| {
            files.extend(
                (0..fresh).map(|i| local(&format!("0x{:X}.txd", 0x9000 + i), 1, Some(at(2000)))),
            );
            files
        };
        // 101 stale of 101: refused.
        let wipe = plan(&list, &old(101));
        assert!(wipe.stale.is_empty());
        assert_eq!(wipe.kept, 101);
        // 101 stale of 202 (exactly half, not more): allowed.
        assert_eq!(plan(&list, &with_fresh(old(101), 101)).stale.len(), 101);
        // 101 stale of 201 (more than half): refused.
        assert!(plan(&list, &with_fresh(old(101), 100)).stale.is_empty());
        // 100 stale of 100: not more than 100, allowed.
        assert_eq!(plan(&list, &old(100)).stale.len(), 100);
        assert!(!looks_like_a_wipe(5, 5));
        assert!(looks_like_a_wipe(11_000, 11_414));
    }

    #[test]
    fn untrusted_lists_never_delete() {
        let old = [local("0xD1.dff", 5, Some(at(1)))];
        // Empty list.
        let empty = list_of(vec![], Some(at(1000)));
        assert_eq!(plan(&empty, &old).stale, Vec::<String>::new());
        assert_eq!(plan(&empty, &old).kept, 1);
        // No zip date.
        let undated = list_of(vec![entry("0xA.dff", 1)], None);
        assert!(plan(&undated, &old).stale.is_empty());
        // Bad entries were dropped.
        let mut partial = list_of(vec![entry("0xA.dff", 1)], Some(at(1000)));
        partial.rejected = 1;
        assert!(plan(&partial, &old).stale.is_empty());
    }

    #[test]
    fn scan_only_sees_plain_cache_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("0xA.dff"), b"aaaa").unwrap();
        fs::write(root.join("0xb.dff"), b"lower").unwrap();
        fs::write(root.join("readme.txt"), b"x").unwrap();
        fs::write(root.join("0xA.dff.0a1b.sarp-part"), b"half").unwrap();
        fs::create_dir(root.join("0xF.dff")).unwrap(); // a folder named like a file
        fs::write(root.join("0xF.dff").join("0x1.dff"), b"nested").unwrap();

        let scan = scan(root).unwrap();
        assert_eq!(scan.files.len(), 1);
        assert_eq!(scan.files[0].name, "0xA.dff");
        assert_eq!(scan.files[0].size, 4);
        assert!(scan.files[0].modified.is_some());
        assert_eq!(scan.temps, vec!["0xA.dff.0a1b.sarp-part"]);
        assert!(super::scan(&root.join("missing")).unwrap().files.is_empty());
    }

    #[test]
    fn links_are_never_followed_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        let cache = dir.path().join("cache");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(outside.join("precious.bin"), b"keep me").unwrap();
        // Symlinks need Developer Mode or admin on Windows: skip if denied.
        let link = cache.join("0xABC.dff");
        if std::os::windows::fs::symlink_file(outside.join("precious.bin"), &link).is_err() {
            eprintln!("symlinks not available, skipping");
            return;
        }
        assert!(scan(&cache).unwrap().files.is_empty());
        assert_eq!(
            remove_files(&cache, &["0xABC.dff".into()], is_cache_name),
            0
        );
        assert!(outside.join("precious.bin").exists());
    }

    #[test]
    fn junctions_are_never_followed_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        let cache = dir.path().join("cache");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&cache).unwrap();
        // A folder full of cache-named files that is not the cache folder.
        fs::write(outside.join("0x1.dff"), b"keep me").unwrap();
        // Junctions need no privileges.
        let junction = cache.join("0xABC.dff");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");

        let list = list_of(vec![entry("0x1.dff", 7)], Some(SystemTime::now()));
        let scanned = scan(&cache).unwrap();
        assert!(scanned.files.is_empty());
        assert!(plan(&list, &scanned.files).stale.is_empty());
        assert_eq!(
            remove_files(&cache, &["0xABC.dff".into()], is_cache_name),
            0
        );
        assert!(junction.exists());
        assert!(outside.join("0x1.dff").exists());
    }

    #[test]
    fn remove_files_rechecks_names_and_clears_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("0xA.dff"), b"a").unwrap();
        fs::write(root.join("0xB.dff"), b"b").unwrap();
        fs::write(root.join("keep.txt"), b"k").unwrap();
        let mut ro = fs::metadata(root.join("0xB.dff")).unwrap().permissions();
        ro.set_readonly(true);
        fs::set_permissions(root.join("0xB.dff"), ro).unwrap();

        let names = ["0xA.dff", "0xB.dff", "keep.txt", "0xMISSING.dff", "..\\x"].map(String::from);
        assert_eq!(remove_files(root, &names, is_cache_name), 2);
        assert!(!root.join("0xA.dff").exists());
        assert!(!root.join("0xB.dff").exists());
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn space_check() {
        assert!(check_space(100, None).is_ok());
        assert!(check_space(100, Some(SPACE_MARGIN + 100)).is_ok());
        let error = check_space(2 * 1024 * 1024 * 1024, Some(1024 * 1024 * 1024)).unwrap_err();
        assert_eq!(
            error,
            "No hay espacio suficiente en el disco: el caché necesita 2.2 GB libres y hay 1.0 GB. \
             Libera espacio y vuelve a intentarlo."
        );
    }

    #[test]
    fn result_shape() {
        let shape = |run| serde_json::to_value(CacheInstallResult::from_run(run)).unwrap();
        // Everything in place.
        assert_eq!(
            shape(Ok(Summary::default())),
            json!({ "ok": true, "failed": 0 })
        );
        // It ran, some files failed: not an error, just counted.
        let some_failed = Summary {
            failed: 2,
            skipped: 1,
            ..Summary::default()
        };
        assert_eq!(shape(Ok(some_failed)), json!({ "ok": false, "failed": 3 }));
        // The user stopped it.
        let cancelled = Summary {
            skipped: 7,
            cancelled: true,
            ..Summary::default()
        };
        assert_eq!(
            shape(Ok(cancelled)),
            json!({ "ok": false, "failed": 7, "cancelled": true })
        );
        // It could not run (no list, no space, game open...).
        assert_eq!(
            shape(Err("x".into())),
            json!({ "ok": false, "error": "x", "failed": 0 })
        );
    }

    #[test]
    fn missing_files_are_counted_not_errors() {
        let gave_up = Summary {
            downloaded: 10,
            failed: 24,
            skipped: 100,
            ..Summary::default()
        };
        assert_eq!(gave_up.missing(), 124);
        assert_eq!(Summary::default().missing(), 0);
        assert_eq!(
            missing_message(1),
            "Falta 1 archivo del caché; el juego lo descargará al entrar."
        );
        assert_eq!(
            missing_message(124),
            "Faltan 124 archivos del caché; el juego los descargará al entrar."
        );
    }

    #[test]
    fn busy_flag_allows_one_sync_and_cancels_it() {
        let state = CacheState::default();
        assert!(!state.is_syncing());
        assert!(!state.cancel(), "nothing to cancel");
        let first = state.begin();
        assert!(first.is_some());
        assert!(state.is_syncing());
        assert!(state.begin().is_none());
        let token = first.as_ref().unwrap().cancel.clone();
        assert!(!token.is_cancelled());
        assert!(state.cancel());
        assert!(token.is_cancelled());
        drop(first);
        assert!(!state.is_syncing());
        assert!(!state.cancel());
        // A new sync gets a fresh token.
        let second = state.begin().unwrap();
        assert!(!second.cancel.is_cancelled());
    }
}
