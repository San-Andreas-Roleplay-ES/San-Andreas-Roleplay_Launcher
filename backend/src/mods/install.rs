//! Putting mods into the game folder and taking them out.
//!
//! A run goes in steps, so nothing half-checked ever reaches the game:
//!
//! 1. **Stage** each variant to install: download its package (resumable,
//!    from the catalog's URL, never with `X-Launcher-Key`), check the zip's
//!    SHA-256, unpack only the files the catalog lists into a temp folder,
//!    checking each one's size and hash.
//! 2. **Plan** the file operations of every step of the run. A file the
//!    launcher did not install itself is copied to the backups folder before
//!    anything overwrites it (and goes back on uninstall).
//! 3. **Apply** them: directly, or, when the folder is protected (Program
//!    Files), all of them in one elevated PowerShell script: one UAC prompt
//!    for the whole run.
//! 4. **Verify** the folder and record what is now ours, file by file, so a
//!    step that failed half-way is still known exactly (and shows as damaged
//!    or outdated next time).
//!
//! Every path is resolved through `disk::confined`: inside the game folder,
//! never through a symlink or junction.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::catalog::{Catalog, Mod, Variant};
use super::disk::{confined, exists, Hasher};
use super::record::{ModRecord, RecordedFile};
use super::status::Work;
use crate::game::paths::REQUIRED_DIRS;
use crate::install::cache::with_writable;
use crate::install::cancel::Cancel;
use crate::install::fs::{random_hex, remove_zone_identifier, sha256_file_async};
use crate::install::powershell::{self, quote, Outcome, EXIT_UAC_CANCELLED};
use crate::install::progress::{Phase, Progress, Reporter};
use crate::net::http::{download_with_resume, Leftover, Limits, ResumeOptions};

/// Under the temp folder; swept by `install::fs::sweep_temp` (`sarp-mods-`)
/// when nothing touched it for a day.
pub const PACKAGES: &str = "sarp-mods-zip";
const TEMP_SUFFIX: &str = ".sarp-part";
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);
const READ_CHUNK: usize = 64 * 1024;
pub const UAC_CANCELLED: &str = "Permiso de administrador cancelado.";
const DAMAGED: &str = "Descarga dañada.";

/// Progress for one mod (`Some(id)`) or for the run as a whole.
pub type ModReporter = Arc<dyn Fn(Option<&str>, Progress) + Send + Sync>;

/// Everything a run needs besides the catalog and the records.
#[derive(Clone)]
pub struct Job {
    /// No redirects, no `X-Launcher-Key`.
    pub client: Client,
    /// The game folder.
    pub dir: PathBuf,
    /// This folder's backups (`mod-backups/<hash of the folder>`).
    pub backups: PathBuf,
    /// Downloads and staging go under it.
    pub temp: PathBuf,
    pub limits: Limits,
    pub cancel: Cancel,
    pub report: ModReporter,
    pub hasher: Arc<Hasher>,
    /// Whether the launcher may write a folder itself (`can_write_to_dir`).
    pub writable: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    /// Elevate the script when the folder is protected. Tests run it without
    /// (never a UAC prompt), which is how the script itself gets tested.
    pub uac: bool,
}

/// How a run ended for the folder.
#[derive(Debug, Default)]
pub struct Executed {
    /// The folder's records after the run.
    pub records: BTreeMap<String, ModRecord>,
    /// Steps that did not complete, with a message for the user.
    pub failed: Vec<(String, String)>,
    /// Steps that completed.
    pub done: Vec<String>,
    pub cancelled: bool,
}

/// One file operation in the game folder (absolute paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    MakeDir(PathBuf),
    /// Staged or backed-up file → its place (overwriting).
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    Remove(PathBuf),
    /// Same folder, new name.
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    RemoveDirIfEmpty(PathBuf),
}

// ---- paths ----

/// `modloader/radar_hq` → `modloader/.radar_hq`: Modloader skips folders
/// starting with a dot, and it is undone by renaming back.
pub fn dotted(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((parent, name)) => format!("{parent}/.{name}"),
        None => format!(".{rel}"),
    }
}

fn backup_path(backups: &Path, id: &str, rel: &str) -> PathBuf {
    let mut path = backups.join(id);
    path.extend(rel.split('/'));
    path
}

/// The last part of a game-relative path: `modloader/radar_hq` → `radar_hq`.
pub fn leaf(rel: &str) -> &str {
    let rel = rel.trim_end_matches('/');
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Folders above `rel`, shallowest first: `a/b/c.txd` → `a`, `a/b`.
fn ancestors(rel: &str) -> Vec<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    (1..parts.len()).map(|n| parts[..n].join("/")).collect()
}

/// Folders never removed, even when the launcher created them and they end
/// up empty: `modloader/` (users keep their own mods there) and the game's.
fn kept_dir(rel: &str) -> bool {
    rel.eq_ignore_ascii_case("modloader")
        || REQUIRED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(rel))
}

/// `rel` with its last part spelled as it is on disk (Windows names ignore
/// case), so renaming a user's folder keeps their spelling.
fn on_disk(dir: &Path, rel: &str) -> String {
    let (parent, name) = match rel.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None => (None, rel),
    };
    let folder = match parent {
        Some(parent) => confined(dir, parent),
        None => Some(dir.to_path_buf()),
    };
    let actual = folder
        .and_then(|f| fs::read_dir(f).ok())
        .and_then(|entries| {
            entries.flatten().find_map(|entry| {
                let found = entry.file_name().to_string_lossy().into_owned();
                found.eq_ignore_ascii_case(name).then_some(found)
            })
        });
    match (parent, actual) {
        (Some(parent), Some(found)) => format!("{parent}/{found}"),
        (None, Some(found)) => found,
        _ => rel.to_string(),
    }
}

fn confine(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    confined(dir, rel).ok_or_else(|| format!("Ruta no permitida: {rel}"))
}

/// Deletes a file of ours (a backup, a temp), read-only or not.
fn delete(path: &Path) {
    let _ = with_writable(path, || fs::remove_file(path));
}

// ---- stage ----

/// A variant ready to go into place: its files in a temp folder.
struct Staged {
    id: String,
    variant: Variant,
    folder: PathBuf,
    /// The staged copy of each of `variant.files`, same order.
    files: Vec<PathBuf>,
}

/// The package's entries that are listed files, by lower-case name with
/// forward slashes. Everything else in the zip is ignored.
fn unpack(
    zip: &Path,
    folder: &Path,
    variant: &Variant,
    cancel: &Cancel,
) -> Result<Vec<PathBuf>, String> {
    let file = fs::File::open(zip).map_err(|e| format!("open: {e}"))?;
    let mut archive = ZipArchive::new(io::BufReader::new(file)).map_err(|_| DAMAGED.to_string())?;
    let mut index_of = std::collections::HashMap::new();
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index_data(index) else {
            continue;
        };
        let Ok(name) = entry.name() else { continue };
        if entry.is_file() {
            let key = name.replace('\\', "/").to_ascii_lowercase();
            index_of.entry(key).or_insert(index);
        }
    }
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let mut staged = Vec::with_capacity(variant.files.len());
    let mut buf = vec![0u8; READ_CHUNK];
    for (n, file) in variant.files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(crate::net::http::CANCELLED.into());
        }
        let Some(&index) = index_of.get(&file.entry.to_ascii_lowercase()) else {
            log::warn!("mods: package lacks {}", file.entry);
            return Err("Descarga incompleta.".into());
        };
        let mut entry = archive.by_index(index).map_err(|_| DAMAGED.to_string())?;
        // A plain numbered name: the real path only matters in the game folder.
        let out = folder.join(format!("{n}.bin"));
        let mut writer = io::BufWriter::new(fs::File::create(&out).map_err(|e| e.to_string())?);
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        loop {
            let read = entry.read(&mut buf).map_err(|_| DAMAGED.to_string())?;
            if read == 0 {
                break;
            }
            written += read as u64;
            if written > file.bytes {
                break;
            }
            hasher.update(&buf[..read]);
            writer.write_all(&buf[..read]).map_err(|e| e.to_string())?;
        }
        writer.flush().map_err(|e| e.to_string())?;
        if written != file.bytes || hex::encode(hasher.finalize()) != file.sha256 {
            log::warn!(
                "mods: {} in the package does not match the catalog",
                file.entry
            );
            return Err(DAMAGED.into());
        }
        staged.push(out);
    }
    Ok(staged)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("internal error: {e}"))
}

async fn stage(job: &Job, m: &Mod, variant: &Variant) -> Result<Staged, String> {
    let package = &variant.package;
    let packages = job.temp.join(PACKAGES);
    tokio::fs::create_dir_all(&packages).await.map_err(|e| {
        log::warn!("mods: create {}: {e}", packages.display());
        "No se pudo descargar.".to_string()
    })?;
    let zip = packages.join(format!("{}.zip", &package.sha256[..16]));
    let message = format!("Descargando {}...", m.name);
    let id = m.id.clone();
    let report = job.report.clone();
    let progress: Reporter = Arc::new(move |p| report(Some(&id), p));
    let opts = ResumeOptions {
        url: &package.url,
        dest: &zip,
        expected_size: package.bytes,
        leftover: Leftover::Sha256(&package.sha256),
        limits: job.limits,
        message: Some(&message),
    };
    download_with_resume(&job.client, &opts, &progress, &job.cancel).await?;
    (job.report)(
        Some(&m.id),
        Progress::new(Phase::Verify, 100.0).message(format!("Verificando {}...", m.name)),
    );
    let sha = sha256_file_async(zip.clone())
        .await
        .map_err(|e| e.to_string())?;
    if sha != package.sha256 {
        let _ = tokio::fs::remove_file(&zip).await;
        log::warn!("mods: {} package hash does not match the catalog", m.id);
        return Err(DAMAGED.into());
    }
    remove_zone_identifier(&zip);
    (job.report)(
        Some(&m.id),
        Progress::new(Phase::Extract, 0.0).message(format!("Preparando {}...", m.name)),
    );
    let folder = job.temp.join(format!("sarp-launcher-{}", random_hex(8)));
    let (zip_path, staging, wanted, cancel) = (
        zip.clone(),
        folder.clone(),
        variant.clone(),
        job.cancel.clone(),
    );
    let unpacked = blocking(move || unpack(&zip_path, &staging, &wanted, &cancel)).await?;
    // Small and checked: no reason to keep it once unpacked (or refused).
    let _ = tokio::fs::remove_file(&zip).await;
    let _ = tokio::fs::remove_dir(&packages).await;
    match unpacked {
        Ok(files) => Ok(Staged {
            id: m.id.clone(),
            variant: variant.clone(),
            folder,
            files,
        }),
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            Err(error)
        }
    }
}

// ---- plan ----

/// What verification checks after the operations ran.
enum Expect {
    Installed {
        variant: Box<Variant>,
        old: Option<ModRecord>,
        /// Files backed up for this step (lower-case paths).
        backed_up: HashSet<String>,
        /// Old files that were to be removed or restored.
        dropped: Vec<RecordedFile>,
        /// Folders this step creates.
        created: Vec<String>,
    },
    Removed {
        old: ModRecord,
    },
    Conflicts {
        old: ModRecord,
        renamed: Vec<String>,
    },
}

struct Planned {
    id: String,
    ops: Vec<Op>,
    expect: Expect,
}

/// Files other mods' records own in this folder (lower-case).
fn owned_by_others(records: &BTreeMap<String, ModRecord>, id: &str) -> HashSet<String> {
    records
        .iter()
        .filter(|(other, _)| other.as_str() != id)
        .flat_map(|(_, r)| r.files.iter().map(|f| f.path.to_ascii_lowercase()))
        .collect()
}

/// Copies the file at `from` to the backups (replacing a stale copy, even a
/// read-only one).
fn back_up(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if to.exists() {
        with_writable(to, || fs::remove_file(to))?;
    }
    fs::copy(from, to).map(|_| ())
}

/// Operations taking `files` out of the folder, and then the `dirs` the
/// launcher created for them once empty.
///
/// What stays, always: `keep` files, files another mod owns, files the user
/// changed since (no longer our exact bytes), every folder the launcher did
/// not create, `modloader/` and the game's own folders. A replaced original
/// goes back only over our own file (or where it is missing), never over
/// something the user put there since.
fn removal_ops(
    job: &Job,
    id: &str,
    files: &[RecordedFile],
    dirs: &[String],
    others: &HashSet<String>,
) -> Result<Vec<Op>, String> {
    let mut ops = Vec::new();
    for file in files.iter().filter(|f| !f.keep) {
        if others.contains(&file.path.to_ascii_lowercase()) {
            continue;
        }
        let target = confine(&job.dir, &file.path)?;
        let ours = job
            .hasher
            .matches(&job.dir, &file.path, file.bytes, &file.sha256);
        let backup = backup_path(&job.backups, id, &file.path);
        if file.backup && backup.is_file() {
            if ours || !exists(&job.dir, &file.path) {
                ops.push(Op::Copy {
                    from: backup,
                    to: target,
                });
            } else {
                log::info!("mods: {} changed by the user; its backup stays", file.path);
            }
        } else if ours {
            ops.push(Op::Remove(target));
        }
    }
    let mut dirs: Vec<&String> = dirs.iter().filter(|d| !kept_dir(d)).collect();
    // Deepest first, so a folder empties before its parent is tried.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for dir in dirs {
        ops.push(Op::RemoveDirIfEmpty(confine(&job.dir, dir)?));
    }
    Ok(ops)
}

fn plan_install(
    job: &Job,
    staged: &Staged,
    old: Option<&ModRecord>,
    records: &BTreeMap<String, ModRecord>,
) -> Result<Planned, String> {
    let id = staged.id.as_str();
    let others = owned_by_others(records, id);
    let mut ops = Vec::new();
    let mut backed_up = HashSet::new();
    let mut made: HashSet<PathBuf> = HashSet::new();
    let mut created: Vec<String> = Vec::new();
    for (file, source) in staged.variant.files.iter().zip(&staged.files) {
        let target = confine(&job.dir, &file.path)?;
        let present = exists(&job.dir, &file.path);
        if file.keep && present {
            continue;
        }
        let ours = old.is_some_and(|r| r.owns(&file.path).is_some())
            || others.contains(&file.path.to_ascii_lowercase());
        if present && !ours && !file.keep {
            let backup = backup_path(&job.backups, id, &file.path);
            back_up(&target, &backup).map_err(|e| {
                log::warn!("mods: back up {}: {e}", target.display());
                format!("No se pudo respaldar {}.", leaf(&file.path))
            })?;
            backed_up.insert(file.path.to_ascii_lowercase());
        }
        if !file.keep
            && job
                .hasher
                .matches(&job.dir, &file.path, file.bytes, &file.sha256)
        {
            // Already the right content.
            continue;
        }
        // Folders that don't exist yet are ours: only those may go later.
        for folder in ancestors(&file.path) {
            let new = !exists(&job.dir, &format!("{folder}/"))
                && !created.iter().any(|c| c.eq_ignore_ascii_case(&folder));
            if new {
                created.push(folder);
            }
        }
        if let Some(parent) = target
            .parent()
            .filter(|p| !p.exists() && made.insert(p.to_path_buf()))
        {
            ops.push(Op::MakeDir(parent.to_path_buf()));
        }
        ops.push(Op::Copy {
            from: source.clone(),
            to: target,
        });
    }
    let listed: HashSet<String> = staged
        .variant
        .files
        .iter()
        .map(|f| f.path.to_ascii_lowercase())
        .collect();
    let dropped: Vec<RecordedFile> = old
        .map(|r| {
            r.files
                .iter()
                .filter(|f| !listed.contains(&f.path.to_ascii_lowercase()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    // Our folders the new files no longer use.
    let unused: Vec<String> = old
        .map(|r| {
            r.dirs
                .iter()
                .filter(|d| {
                    !listed
                        .iter()
                        .any(|p| p.starts_with(&format!("{}/", d.to_ascii_lowercase())))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    ops.extend(removal_ops(job, id, &dropped, &unused, &others)?);
    Ok(Planned {
        id: id.to_string(),
        ops,
        expect: Expect::Installed {
            variant: Box::new(staged.variant.clone()),
            old: old.cloned(),
            backed_up,
            dropped,
            created,
        },
    })
}

fn plan_removal(
    job: &Job,
    id: &str,
    old: &ModRecord,
    records: &BTreeMap<String, ModRecord>,
) -> Result<Planned, String> {
    let others = owned_by_others(records, id);
    let mut ops = removal_ops(job, id, &old.files, &old.dirs, &others)?;
    for rel in &old.disabled {
        if exists(&job.dir, &dotted(rel)) && !exists(&job.dir, rel) {
            ops.push(Op::Rename {
                from: confine(&job.dir, &dotted(rel))?,
                to: confine(&job.dir, rel)?,
            });
        }
    }
    Ok(Planned {
        id: id.to_string(),
        ops,
        expect: Expect::Removed { old: old.clone() },
    })
}

fn plan_conflicts(job: &Job, m: &Mod, old: &ModRecord) -> Result<Planned, String> {
    let mut ops = Vec::new();
    let mut renamed = Vec::new();
    for rel in m.replaces.iter().filter(|rel| exists(&job.dir, rel)) {
        // The user's own spelling (`Radar_HQ`) is kept both ways.
        let actual = on_disk(&job.dir, rel);
        if exists(&job.dir, &dotted(&actual)) {
            return Err(format!("Ya existe {}.", leaf(&dotted(&actual))));
        }
        ops.push(Op::Rename {
            from: confine(&job.dir, &actual)?,
            to: confine(&job.dir, &dotted(&actual))?,
        });
        renamed.push(actual);
    }
    Ok(Planned {
        id: m.id.clone(),
        ops,
        expect: Expect::Conflicts {
            old: old.clone(),
            renamed,
        },
    })
}

// ---- apply ----

/// Folders the operations write in (the nearest existing one for folders
/// still to create).
fn touched(ops: &[Op]) -> Vec<PathBuf> {
    let nearest = |path: &Path| {
        let mut dir = path.parent().map(Path::to_path_buf);
        while let Some(candidate) = dir {
            if candidate.is_dir() {
                return Some(candidate);
            }
            dir = candidate.parent().map(Path::to_path_buf);
        }
        None
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for op in ops {
        let path = match op {
            Op::MakeDir(p) | Op::Remove(p) | Op::RemoveDirIfEmpty(p) => p,
            Op::Copy { to, .. } | Op::Rename { to, .. } => to,
        };
        if let Some(dir) = nearest(path) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// The folders can't all be written by the launcher: the run needs an
/// elevated helper (one UAC prompt for all of it).
pub fn needs_elevation(ops: &[Op], writable: &dyn Fn(&Path) -> bool) -> bool {
    touched(ops).iter().any(|dir| !writable(dir))
}

/// Runs the operations as the launcher. Each failure is logged and the rest
/// still run: verification tells what happened.
fn apply(ops: &[Op]) -> usize {
    let mut failed = 0;
    for op in ops {
        let result = match op {
            Op::MakeDir(dir) => fs::create_dir_all(dir),
            Op::Copy { from, to } => copy_into_place(from, to),
            Op::Remove(path) => match with_writable(path, || fs::remove_file(path)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
            Op::Rename { from, to } => fs::rename(from, to),
            Op::RemoveDirIfEmpty(dir) => {
                let _ = fs::remove_dir(dir);
                Ok(())
            }
        };
        if let Err(error) = result {
            log::warn!("mods: {op:?}: {error}");
            failed += 1;
        }
    }
    failed
}

/// Copies to a temp name next to `to` and renames it over, so the game never
/// finds a half-written file.
fn copy_into_place(from: &Path, to: &Path) -> io::Result<()> {
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = to.with_file_name(format!("{name}.{}{TEMP_SUFFIX}", random_hex(4)));
    let result = fs::copy(from, &temp).and_then(|_| with_writable(to, || fs::rename(&temp, to)));
    if result.is_err() {
        delete(&temp);
    }
    result
}

/// The same operations as a PowerShell script (each one on its own: a
/// failure doesn't stop the rest). Exit 0 when all of them ran.
pub fn script(ops: &[Op]) -> String {
    let path = |p: &Path| quote(&p.to_string_lossy());
    let mut lines = vec![
        "$ErrorActionPreference = 'Stop'".to_string(),
        "$failed = 0".to_string(),
    ];
    for op in ops {
        let command = match op {
            Op::MakeDir(dir) => format!("New-Item -ItemType Directory -Force -Path {} | Out-Null", path(dir)),
            Op::Copy { from, to } => {
                format!("Copy-Item -LiteralPath {} -Destination {} -Force", path(from), path(to))
            }
            Op::Remove(p) => format!("Remove-Item -LiteralPath {} -Force", path(p)),
            Op::Rename { from, to } => format!(
                "Rename-Item -LiteralPath {} -NewName {}",
                path(from),
                quote(&to.file_name().unwrap_or_default().to_string_lossy())
            ),
            Op::RemoveDirIfEmpty(dir) => format!(
                "if (-not (Get-ChildItem -LiteralPath {0} -Force)) {{ Remove-Item -LiteralPath {0} -Force }}",
                path(dir)
            ),
        };
        lines.push(format!("try {{ {command} }} catch {{ $failed++ }}"));
    }
    lines.push("if ($failed -gt 0) { exit 1 } else { exit 0 }".into());
    lines.join("\r\n")
}

async fn apply_elevated(ops: &[Op], uac: bool) -> Result<(), String> {
    let script = powershell::write_script("sarp-mods-", &script(ops))
        .await
        .map_err(|e| e.to_string())?;
    let command = if uac {
        powershell::run_script_elevated(&script)
    } else {
        format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"; exit $LASTEXITCODE",
            script.display()
        )
    };
    let outcome = powershell::run(&command, SCRIPT_TIMEOUT).await;
    let _ = tokio::fs::remove_file(&script).await;
    match outcome {
        Outcome::Exited(0) => Ok(()),
        Outcome::Exited(EXIT_UAC_CANCELLED) => Err(UAC_CANCELLED.into()),
        // Partial: verification says which steps made it.
        Outcome::Exited(1) => Ok(()),
        Outcome::Exited(_) => Err("Falló como administrador.".into()),
        Outcome::TimedOut => Err("Tardó demasiado.".into()),
        Outcome::Failed(error) => Err(error),
    }
}

// ---- verify ----

/// The record a step leaves, from what is in the folder now, and whether
/// the step completed.
fn verify(
    job: &Job,
    planned: &Planned,
    records: &mut BTreeMap<String, ModRecord>,
) -> Result<(), String> {
    let dir = &job.dir;
    let ours = |f: &RecordedFile| job.hasher.matches(dir, &f.path, f.bytes, &f.sha256);
    // Old files still ours (a removal that failed), or whose original could
    // not be put back: they stay recorded so a retry finishes the job.
    let leftovers = |files: &[RecordedFile]| -> Vec<RecordedFile> {
        files
            .iter()
            .filter(|f| !f.keep)
            .filter(|f| {
                let backup = backup_path(&job.backups, &planned.id, &f.path);
                if f.backup && backup.is_file() {
                    !exists(dir, &f.path) || ours(f)
                } else {
                    ours(f)
                }
            })
            .cloned()
            .collect()
    };
    // A backup goes only once its original is back in place, byte for byte.
    // One the user's own file kept us from restoring stays where it is.
    let forget = |files: &[RecordedFile], left: &[RecordedFile]| {
        for f in files.iter().filter(|f| f.backup) {
            if left.iter().any(|l| l.path.eq_ignore_ascii_case(&f.path)) {
                continue;
            }
            let backup = backup_path(&job.backups, &planned.id, &f.path);
            let restored = confined(dir, &f.path).is_some_and(|target| {
                let size = |p: &Path| fs::metadata(p).map(|m| m.len()).ok();
                size(&target).is_some()
                    && size(&target) == size(&backup)
                    && job.hasher.sha256(&target).is_some()
                    && job.hasher.sha256(&target) == crate::install::fs::sha256_file(&backup).ok()
            });
            if restored {
                delete(&backup);
            }
        }
    };
    // Our folders that still exist.
    let still = |dirs: &mut Vec<String>| dirs.retain(|d| exists(dir, &format!("{d}/")));
    match &planned.expect {
        Expect::Installed {
            variant,
            old,
            backed_up,
            dropped,
            created,
        } => {
            let mut files = Vec::new();
            let mut missing = None;
            for file in &variant.files {
                let carried = old
                    .as_ref()
                    .and_then(|r| r.owns(&file.path))
                    .is_some_and(|f| f.backup);
                let record = RecordedFile {
                    path: file.path.clone(),
                    bytes: file.bytes,
                    sha256: file.sha256.clone(),
                    keep: file.keep,
                    backup: carried || backed_up.contains(&file.path.to_ascii_lowercase()),
                };
                let in_place = if file.keep {
                    exists(dir, &file.path)
                } else {
                    ours(&record)
                };
                if in_place {
                    files.push(record);
                } else if missing.is_none() {
                    missing = Some(file.path.clone());
                }
            }
            let left = leftovers(dropped);
            forget(dropped, &left);
            let incomplete = !left.is_empty();
            files.extend(left);
            let mut dirs: Vec<String> = old.as_ref().map(|r| r.dirs.clone()).unwrap_or_default();
            for folder in created {
                if !dirs.iter().any(|d| d.eq_ignore_ascii_case(folder)) {
                    dirs.push(folder.clone());
                }
            }
            still(&mut dirs);
            records.insert(
                planned.id.clone(),
                ModRecord {
                    variant: variant.id.clone(),
                    version: variant.version.clone(),
                    files,
                    disabled: old.as_ref().map(|r| r.disabled.clone()).unwrap_or_default(),
                    adopted: false,
                    dirs,
                },
            );
            match missing {
                Some(path) => Err(format!("No se pudo escribir {}.", leaf(&path))),
                None if incomplete => Err("No se pudo quitar la versión anterior.".into()),
                None => Ok(()),
            }
        }
        Expect::Removed { old } => {
            let left = leftovers(&old.files);
            forget(&old.files, &left);
            let disabled: Vec<String> = old
                .disabled
                .iter()
                .filter(|rel| !exists(dir, rel) && exists(dir, &dotted(rel)))
                .cloned()
                .collect();
            if left.is_empty() && disabled.is_empty() {
                // Folders still there hold the user's files now: theirs.
                records.remove(&planned.id);
                prune_empty(&job.backups.join(&planned.id));
                return Ok(());
            }
            let complete = left.is_empty();
            let mut dirs = old.dirs.clone();
            still(&mut dirs);
            records.insert(
                planned.id.clone(),
                ModRecord {
                    files: left,
                    disabled,
                    dirs,
                    ..old.clone()
                },
            );
            if complete {
                Err("No se pudo reactivar lo desactivado.".into())
            } else {
                Err("No se pudo quitar del todo.".into())
            }
        }
        Expect::Conflicts { old, renamed } => {
            let mut record = records
                .get(&planned.id)
                .cloned()
                .unwrap_or_else(|| old.clone());
            let mut failed = None;
            for rel in renamed {
                if !exists(dir, rel) && exists(dir, &dotted(rel)) {
                    if !record.disabled.iter().any(|d| d.eq_ignore_ascii_case(rel)) {
                        record.disabled.push(rel.clone());
                    }
                } else if failed.is_none() {
                    failed = Some(rel.clone());
                }
            }
            records.insert(planned.id.clone(), record);
            match failed {
                Some(rel) => Err(format!("No se pudo desactivar {}.", leaf(&rel))),
                None => Ok(()),
            }
        }
    }
}

// ---- the run ----

fn name_of<'c>(catalog: &'c Catalog, id: &'c str) -> &'c str {
    catalog.get(id).map_or(id, |m| m.name.as_str())
}

/// Runs `works` (in order) against the folder whose records are `records`.
pub async fn execute(
    job: &Job,
    catalog: &Catalog,
    works: &[Work],
    records: BTreeMap<String, ModRecord>,
) -> Executed {
    let mut out = Executed {
        records,
        ..Executed::default()
    };
    let failed_ids = |out: &Executed| -> HashSet<String> {
        out.failed.iter().map(|(id, _)| id.clone()).collect()
    };

    // 1. Stage what gets installed, what it needs first.
    let mut staged: Vec<Staged> = Vec::new();
    for work in works {
        let Work::Install { id, variant } = work else {
            continue;
        };
        let Some((m, v)) = catalog.get(id).and_then(|m| Some((m, m.variant(variant)?))) else {
            out.failed
                .push((id.clone(), "Ya no está disponible.".into()));
            continue;
        };
        let failed = failed_ids(&out);
        if let Some(dep) = m.requires.iter().find(|dep| failed.contains(*dep)) {
            let message = format!("Falló {}.", name_of(catalog, dep));
            out.failed.push((id.clone(), message));
            continue;
        }
        if job.cancel.is_cancelled() {
            out.cancelled = true;
            out.failed
                .push((id.clone(), crate::net::http::CANCELLED.into()));
            continue;
        }
        match stage(job, m, v).await {
            Ok(s) => staged.push(s),
            Err(error) => {
                log::warn!("mods: {id} {variant} not staged: {error}");
                out.cancelled |= job.cancel.is_cancelled();
                out.failed.push((id.clone(), error));
            }
        }
    }

    // 2. Plan (and back up) every step that can still go ahead.
    let skipped = failed_ids(&out);
    let mut planned: Vec<Planned> = Vec::new();
    for work in works.iter().filter(|w| !skipped.contains(w.id())) {
        let id = work.id();
        let old = out.records.get(id);
        let plan = match work {
            Work::Install { .. } => match staged.iter().find(|s| s.id == id) {
                Some(s) => plan_install(job, s, old, &out.records),
                None => continue,
            },
            Work::Remove { .. } => match old {
                Some(old) => plan_removal(job, id, old, &out.records),
                None => continue,
            },
            Work::Conflicts { .. } => match (catalog.get(id), old) {
                (Some(m), Some(old)) => plan_conflicts(job, m, old),
                _ => continue,
            },
        };
        match plan {
            Ok(plan) => planned.push(plan),
            Err(error) => out.failed.push((id.to_string(), error)),
        }
    }

    // 3. Apply, as the launcher or elevated.
    let ops: Vec<Op> = planned.iter().flat_map(|p| p.ops.iter().cloned()).collect();
    let mut run_error = None;
    if !ops.is_empty() {
        let writable = job.writable.clone();
        let elevate = needs_elevation(&ops, &*writable);
        let message = if elevate {
            "Acepta el aviso de Windows."
        } else {
            "Instalando mods..."
        };
        (job.report)(None, Progress::new(Phase::Extract, 0.0).message(message));
        if elevate {
            log::info!("mods: {} operations need elevation", ops.len());
            run_error = apply_elevated(&ops, job.uac).await.err();
        } else {
            let batch = ops.clone();
            let failures = blocking(move || apply(&batch)).await.unwrap_or(usize::MAX);
            if failures > 0 {
                log::warn!("mods: {failures} operations failed");
            }
        }
    }

    // 4. Verify and record.
    for plan in &planned {
        match verify(job, plan, &mut out.records) {
            Ok(()) => out.done.push(plan.id.clone()),
            Err(error) => out
                .failed
                .push((plan.id.clone(), run_error.clone().unwrap_or(error))),
        }
    }
    for s in staged {
        let _ = tokio::fs::remove_dir_all(&s.folder).await;
    }
    prune_empty(&job.backups);
    out
}

/// Removes the empty folders under `dir`, then `dir` if it ends up empty.
/// Links are never followed.
fn prune_empty(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                prune_empty(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_names_ancestors_and_kept_folders() {
        assert_eq!(dotted("modloader/radar_hq"), "modloader/.radar_hq");
        assert_eq!(dotted("dinput8.dll"), ".dinput8.dll");
        assert_eq!(leaf("modloader/radar_hq/"), "radar_hq");
        assert_eq!(leaf("x.asi"), "x.asi");
        assert_eq!(
            ancestors("modloader/radar/a/x.txd"),
            ["modloader", "modloader/radar", "modloader/radar/a"]
        );
        assert!(ancestors("x.asi").is_empty());
        for kept in ["modloader", "MODLOADER", "models", "data", "anim", "audio"] {
            assert!(kept_dir(kept), "{kept}");
        }
        assert!(!kept_dir("modloader/radar"));
    }

    #[test]
    fn on_disk_keeps_the_users_spelling() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("modloader").join("Radar_HQ")).unwrap();
        assert_eq!(
            on_disk(dir.path(), "modloader/radar_hq"),
            "modloader/Radar_HQ"
        );
        assert_eq!(
            on_disk(dir.path(), "modloader/missing"),
            "modloader/missing"
        );
        fs::write(dir.path().join("DInput8.dll"), b"x").unwrap();
        assert_eq!(on_disk(dir.path(), "dinput8.dll"), "DInput8.dll");
    }

    #[test]
    fn elevation_is_needed_when_any_touched_folder_is_protected() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("GTA");
        fs::create_dir_all(game.join("modloader")).unwrap();
        let ops = vec![
            Op::MakeDir(game.join("modloader").join("radar")),
            Op::Copy {
                from: root.path().join("0.bin"),
                to: game.join("modloader").join("radar").join("x.txd"),
            },
            Op::Copy {
                from: root.path().join("1.bin"),
                to: game.join("vorbisFile.dll"),
            },
        ];
        // The new folder is judged by the nearest one that exists.
        assert_eq!(touched(&ops), [game.join("modloader"), game.clone()]);
        assert!(!needs_elevation(&ops, &|_| true));
        assert!(needs_elevation(&ops, &|_| false));
        let protected = game.clone();
        assert!(needs_elevation(&ops, &move |dir: &Path| dir != protected));
        assert!(!needs_elevation(&[], &|_| false));
    }

    #[test]
    fn script_shape_and_quoting() {
        let ops = vec![
            Op::MakeDir(PathBuf::from(r"C:\Program Files\GTA\modloader\radar")),
            Op::Copy {
                from: PathBuf::from(r"C:\Users\Dan's\Temp\0.bin"),
                to: PathBuf::from(r"C:\Program Files\GTA\modloader\radar\x.txd"),
            },
            Op::Remove(PathBuf::from(r"C:\Program Files\GTA\old.asi")),
            Op::Rename {
                from: PathBuf::from(r"C:\Program Files\GTA\modloader\radar_hq"),
                to: PathBuf::from(r"C:\Program Files\GTA\modloader\.radar_hq"),
            },
            Op::RemoveDirIfEmpty(PathBuf::from(r"C:\Program Files\GTA\modloader\old")),
        ];
        let text = script(&ops);
        assert!(text.contains(r"try { New-Item -ItemType Directory -Force -Path 'C:\Program Files\GTA\modloader\radar' | Out-Null } catch { $failed++ }"));
        assert!(text.contains(r"Copy-Item -LiteralPath 'C:\Users\Dan''s\Temp\0.bin' -Destination 'C:\Program Files\GTA\modloader\radar\x.txd' -Force"));
        assert!(text.contains(r"Remove-Item -LiteralPath 'C:\Program Files\GTA\old.asi' -Force"));
        assert!(text.contains(r"Rename-Item -LiteralPath 'C:\Program Files\GTA\modloader\radar_hq' -NewName '.radar_hq'"));
        assert!(text.contains(
            r"if (-not (Get-ChildItem -LiteralPath 'C:\Program Files\GTA\modloader\old' -Force))"
        ));
        assert!(text.ends_with("if ($failed -gt 0) { exit 1 } else { exit 0 }"));
    }
}
