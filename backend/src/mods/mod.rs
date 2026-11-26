//! Mods the launcher installs and manages, all of them from a signed catalog
//! the API relays (`source`): required ones (Modloader, an ASI loader) by
//! itself, optional ones as the user chooses in the Mods panel, with a
//! variant when a mod has several (radar: standard or HD).
//!
//! - `signature` / `source`: the catalog is checked (minisign, the updater's
//!   key) before it is read, can't roll back to an older `serial`, and the
//!   last good one is kept for when the API can't be reached.
//! - `catalog`: its rules; a bad mod is skipped, never the whole catalog.
//! - `status`: what each mod looks like in the game folder (ours, someone
//!   else's install, damaged...) and what should happen to it.
//! - `install`: downloads, checks and puts files in place (elevated in one
//!   go when the folder is protected), with backups of anything replaced.
//! - `record`: what we installed, per game folder, and the user's choices.

pub mod catalog;
mod disk;
pub mod install;
pub mod record;
mod signature;
mod source;
mod status;

#[cfg(test)]
mod engine_tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use reqwest::Client;
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::app::events::{self, MODS_INSTALL_PROGRESS};
use crate::config::{mods_pubkey, IS_DEV};
use crate::game::launcher::find_game_process;
use crate::game::paths::game_dir;
use crate::install::cancel::Cancel;
use crate::install::fs::can_write_to_dir;
use crate::install::progress::{Phase, Progress};
use crate::net::api::Api;
use crate::net::http::Limits;
use crate::store::Store;
use crate::Ctx;

use catalog::{Catalog, Mod};
use disk::Hasher;
use install::{Job, ModReporter};
use record::{folder_key, ModPref, ModRecord};
use source::{Loaded, Origin, Problem};
use status::{Action, Desired, Inputs, State, Survey, View, Work};

/// The health check and the run it may start share one catalog fetch.
const CATALOG_TTL: Duration = Duration::from_secs(60);

// ---- payloads ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CatalogState {
    /// Fresh from the API.
    Live,
    /// The last good one (see `detail`).
    Saved,
    /// No mods published yet.
    Empty,
    /// No usable catalog (see `detail`): nothing installs.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInfo {
    pub state: CatalogState,
    /// For the user, when something is off. Never blocks playing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantInfo {
    pub id: String,
    pub label: String,
    pub version: String,
    /// Download size.
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledInfo {
    pub variant: String,
    pub version: String,
    /// Found in place rather than installed by the launcher.
    pub adopted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub required: bool,
    /// Wanted on (always, for a required mod).
    pub enabled: bool,
    /// The variant wanted.
    pub variant: String,
    pub variants: Vec<VariantInfo>,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledInfo>,
    /// What the automatic flow would do.
    pub action: Action,
    /// Why it is blocked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The last attempt this session failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub installing: bool,
    /// Required, its install failed and it is not in place: Play waits.
    pub blocking: bool,
    /// `replaces` paths present (offered to disable).
    pub conflicts: Vec<String>,
    /// `replaces` paths it renamed out of the way.
    pub disabled_conflicts: Vec<String>,
    /// Names of the enabled mods that need this one (it can't be turned off).
    pub required_by: Vec<String>,
    /// Names of the mods it needs.
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModsHealth {
    pub catalog: CatalogInfo,
    /// There is a valid game folder to check against.
    pub ready: bool,
    pub mods: Vec<ModStatus>,
    /// The automatic flow has something to do (`sync_mods`).
    pub pending: bool,
    pub busy: bool,
    /// A required mod failed to install: Play waits for it.
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModFailure {
    pub id: String,
    pub name: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModsSyncResult {
    pub ok: bool,
    /// The run could not happen at all (or the request was refused).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub failed: Vec<ModFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
}

impl ModsSyncResult {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            failed: Vec::new(),
            cancelled: None,
        }
    }

    fn refused(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Self::ok()
        }
    }
}

/// `mods:install:progress`: the install progress, for one mod (`id`) or for
/// the run as a whole.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModsProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub progress: Progress,
}

// ---- state ----

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// What the mods commands share.
#[derive(Default)]
pub struct ModsState {
    loaded: tokio::sync::Mutex<Option<(Instant, Arc<Loaded>)>>,
    busy: AtomicBool,
    running: Mutex<Option<Cancel>>,
    /// The mods of the run in progress.
    installing: Mutex<HashSet<String>>,
    /// Mods whose last attempt this session failed, with why. The automatic
    /// flow leaves them alone until a retry.
    errors: Mutex<HashMap<String, String>>,
    blocking: AtomicBool,
    hasher: Arc<Hasher>,
}

struct Busy<'a>(&'a ModsState);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *lock(&self.0.running) = None;
        lock(&self.0.installing).clear();
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

impl ModsState {
    /// The catalog, fetched at most once per `CATALOG_TTL` (always with
    /// `force`). An unreachable API is not remembered: the next call asks
    /// again.
    pub async fn loaded(&self, api: &Api, store: &Store, force: bool) -> Arc<Loaded> {
        let mut slot = self.loaded.lock().await;
        if let Some((at, loaded)) = slot.as_ref().filter(|_| !force) {
            if at.elapsed() < CATALOG_TTL {
                return loaded.clone();
            }
        }
        let loaded = Arc::new(match signature::public_key(&mods_pubkey()) {
            Ok(key) => source::load(api, store, &key, !IS_DEV).await,
            Err(error) => {
                log::error!("mods: unusable public key: {error}");
                Loaded {
                    catalog: None,
                    origin: Origin::Missing,
                    problem: Some(Problem::BadSignature),
                }
            }
        });
        *slot = (loaded.problem != Some(Problem::Unreachable))
            .then(|| (Instant::now(), loaded.clone()));
        loaded
    }

    fn begin(&self) -> Option<Busy<'_>> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(Busy(self))
    }

    /// Mod files are being written: the game must not start meanwhile.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// A required mod failed to install (last check): Play waits.
    pub fn is_blocking(&self) -> bool {
        self.blocking.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) -> bool {
        match lock(&self.running).as_ref() {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    fn errors(&self) -> HashMap<String, String> {
        lock(&self.errors).clone()
    }
}

// ---- the engine ----

/// What one request may ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Everything the automatic flow does (required mods, defaults, updates,
    /// repairs). `retry`: also the mods that failed this session.
    Auto {
        retry: bool,
    },
    Enable {
        id: String,
        enabled: bool,
    },
    Variant {
        id: String,
        variant: String,
    },
    /// Install ours over an install made by other means.
    Recommended {
        id: String,
    },
    /// Rename the mod's `replaces` out of the way.
    Conflicts {
        id: String,
    },
}

/// Everything the mods logic touches, so tests can run it on temp folders.
pub struct Engine<'a> {
    pub store: &'a Store,
    pub state: &'a ModsState,
    /// The game folder (`None`: no valid one yet).
    pub dir: Option<PathBuf>,
    pub client: Client,
    /// `mod-backups`, one subfolder per game folder.
    pub backups: PathBuf,
    pub temp: PathBuf,
    pub limits: Limits,
    pub report: ModReporter,
    pub writable: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    pub uac: bool,
    pub launcher: Version,
    pub game_running: Arc<dyn Fn() -> bool + Send + Sync>,
}

fn catalog_info(loaded: &Loaded) -> CatalogInfo {
    let serial = loaded.catalog.as_ref().map(|c| c.serial);
    let detail = match (loaded.origin, loaded.problem) {
        (Origin::Live, _) | (Origin::Saved, None) | (Origin::Empty, _) => None,
        (Origin::Saved, Some(Problem::Unreachable)) => {
            Some("No se pudo actualizar la lista de mods; se usa la última descargada.")
        }
        (Origin::Saved, Some(Problem::TooNew)) => {
            Some("Hay mods nuevos que necesitan una versión más nueva del launcher.")
        }
        (Origin::Saved, Some(_)) => Some(
            "La lista de mods recibida no pasó la verificación de seguridad; se usa la última verificada.",
        ),
        (Origin::Missing, Some(Problem::TooNew)) => {
            Some("Actualiza el launcher para ver los mods disponibles.")
        }
        (Origin::Missing, Some(Problem::Unreachable) | None) => {
            Some("No se pudo obtener la lista de mods. Revisa tu conexión a Internet.")
        }
        (Origin::Missing, Some(_)) => Some(
            "La lista de mods recibida no pasó la verificación de seguridad: no se instaló nada.",
        ),
    };
    CatalogInfo {
        state: match loaded.origin {
            Origin::Live => CatalogState::Live,
            Origin::Saved => CatalogState::Saved,
            Origin::Empty => CatalogState::Empty,
            Origin::Missing => CatalogState::Missing,
        },
        detail: detail.map(String::from),
        serial,
    }
}

fn wanted_of(survey: &Survey) -> HashMap<String, Desired> {
    survey
        .views
        .iter()
        .map(|v| (v.id.clone(), v.desired.clone()))
        .collect()
}

fn name_of(catalog: &Catalog, id: &str) -> String {
    catalog
        .get(id)
        .map_or_else(|| id.to_string(), |m| m.name.clone())
}

/// A list of names in Spanish: "A", "A y B", "A, B y C".
fn names(list: &[String]) -> String {
    match list {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} y {last}", rest.join(", ")),
    }
}

impl Engine<'_> {
    fn records(&self) -> BTreeMap<String, ModRecord> {
        let Some(dir) = &self.dir else {
            return BTreeMap::new();
        };
        self.store
            .get()
            .mods_installed
            .get(&folder_key(dir))
            .map(|i| i.0.clone())
            .unwrap_or_default()
    }

    fn save_records(&self, records: BTreeMap<String, ModRecord>) {
        let Some(dir) = &self.dir else { return };
        let key = folder_key(dir);
        if self.store.get().mods_installed.get(&key).map(|i| &i.0) == Some(&records) {
            return;
        }
        self.store.update(|d| {
            if records.is_empty() {
                d.mods_installed.remove(&key);
            } else {
                d.mods_installed.insert(key, record::Installed(records));
            }
        });
    }

    /// This folder's backups: `mod-backups/<hash of the folder>`.
    fn backups_dir(&self, dir: &Path) -> PathBuf {
        let hash = Sha256::digest(folder_key(dir).as_bytes());
        self.backups.join(&hex::encode(hash)[..16])
    }

    /// Looks at the folder (on the blocking pool) and saves what it adopted.
    async fn survey(&self, catalog: &Arc<Catalog>) -> Survey {
        let (catalog, dir, records, prefs, launcher, hasher) = (
            catalog.clone(),
            self.dir.clone(),
            self.records(),
            self.store.get().mods_prefs,
            self.launcher.clone(),
            self.state.hasher.clone(),
        );
        let survey = tokio::task::spawn_blocking(move || {
            status::survey(
                &Inputs {
                    catalog: &catalog,
                    dir,
                    records: &records,
                    prefs: &prefs,
                    launcher: &launcher,
                },
                &hasher,
            )
        })
        .await
        .unwrap_or_default();
        if !survey.adopted.is_empty() {
            let mut records = self.records();
            records.extend(survey.adopted.iter().cloned());
            self.save_records(records);
        }
        if !survey.prefs.is_empty() {
            self.store
                .update(|d| d.mods_prefs.extend(survey.prefs.iter().cloned()));
        }
        survey
    }

    fn status(&self, catalog: &Catalog, m: &Mod, view: &View, survey: &Survey) -> ModStatus {
        let errors = self.state.errors();
        let error = errors.get(&m.id).cloned();
        let blocking = m.required
            && error.is_some()
            && matches!(view.state, State::NotInstalled | State::Damaged);
        ModStatus {
            id: m.id.clone(),
            name: m.name.clone(),
            description: m.description.clone(),
            category: m.category.clone(),
            required: m.required,
            enabled: view.desired.enabled,
            variant: view.desired.variant.clone(),
            variants: m
                .variants
                .iter()
                .map(|v| VariantInfo {
                    id: v.id.clone(),
                    label: v.label.clone(),
                    version: v.version.clone(),
                    bytes: v.package.bytes,
                })
                .collect(),
            state: view.state,
            installed: view.record.as_ref().map(|r| InstalledInfo {
                variant: r.variant.clone(),
                version: r.version.clone(),
                adopted: r.adopted,
            }),
            action: view.action,
            detail: view.blocked.clone(),
            error,
            installing: lock(&self.state.installing).contains(&m.id),
            blocking,
            conflicts: view.conflicts.clone(),
            disabled_conflicts: view
                .record
                .as_ref()
                .map(|r| r.disabled.clone())
                .unwrap_or_default(),
            required_by: status::dependents(catalog, &wanted_of(survey), &m.id)
                .into_iter()
                .map(|d| d.name.clone())
                .collect(),
            requires: m.requires.iter().map(|r| name_of(catalog, r)).collect(),
        }
    }

    /// The Mods row and panel.
    pub async fn check(&self, loaded: &Loaded) -> ModsHealth {
        let info = catalog_info(loaded);
        let Some(catalog) = loaded.catalog.as_ref() else {
            self.state.blocking.store(false, Ordering::SeqCst);
            return ModsHealth {
                catalog: info,
                ready: self.dir.is_some(),
                mods: Vec::new(),
                pending: false,
                busy: self.state.is_busy(),
                blocking: false,
            };
        };
        let survey = self.survey(catalog).await;
        let errors = self.state.errors();
        let mods: Vec<ModStatus> = catalog
            .mods
            .iter()
            .zip(&survey.views)
            .map(|(m, view)| self.status(catalog, m, view, &survey))
            .collect();
        let blocking = mods.iter().any(|m| m.blocking);
        self.state.blocking.store(blocking, Ordering::SeqCst);
        ModsHealth {
            catalog: info,
            ready: self.dir.is_some(),
            pending: self.dir.is_some()
                && survey
                    .views
                    .iter()
                    .any(|v| v.action != Action::None && !errors.contains_key(&v.id)),
            busy: self.state.is_busy(),
            blocking,
            mods,
        }
    }

    /// Changes the user's choices for `request` (checked against the
    /// catalog) and says which mods the run may touch. `Err`: refused.
    fn choose(
        &self,
        catalog: &Catalog,
        survey: &Survey,
        request: &Request,
    ) -> Result<(Option<HashSet<String>>, Vec<Work>), String> {
        let find = |id: &str| -> Result<(&Mod, &View), String> {
            catalog
                .get(id)
                .zip(survey.view(id))
                .ok_or_else(|| "Ese mod ya no está en la lista.".to_string())
        };
        let mut prefs = self.store.get().mods_prefs;
        let with_deps = |id: &str| {
            let mut only = status::dependencies(catalog, id);
            only.insert(id.to_string());
            only
        };
        let chosen = match request {
            Request::Auto { .. } => (None, Vec::new()),
            Request::Enable { id, enabled: false } => {
                let (m, view) = find(id)?;
                if m.required {
                    return Err(format!("{} es esencial: no se puede desactivar.", m.name));
                }
                let needers: Vec<String> = status::dependents(catalog, &wanted_of(survey), id)
                    .into_iter()
                    .map(|d| d.name.clone())
                    .collect();
                if !needers.is_empty() {
                    return Err(format!(
                        "Primero desactiva {}: lo necesita.",
                        names(&needers)
                    ));
                }
                prefs.insert(
                    id.clone(),
                    ModPref {
                        enabled: false,
                        variant: Some(view.desired.variant.clone()),
                    },
                );
                (Some(HashSet::from([id.clone()])), Vec::new())
            }
            Request::Enable { id, enabled: true } => {
                let (_, view) = find(id)?;
                if let Some(reason) = &view.blocked {
                    return Err(reason.clone());
                }
                let only = with_deps(id);
                for dep in &only {
                    if let Some(dep_view) = survey.view(dep) {
                        prefs.insert(
                            dep.clone(),
                            ModPref {
                                enabled: true,
                                variant: Some(dep_view.desired.variant.clone()),
                            },
                        );
                    }
                }
                (Some(only), Vec::new())
            }
            Request::Variant { id, variant } => {
                let (m, view) = find(id)?;
                if m.variant(variant).is_none() {
                    return Err("Esa variante no existe.".into());
                }
                prefs.insert(
                    id.clone(),
                    ModPref {
                        enabled: view.desired.enabled,
                        variant: Some(variant.clone()),
                    },
                );
                let only = if view.desired.enabled {
                    with_deps(id)
                } else {
                    HashSet::from([id.clone()])
                };
                (Some(only), Vec::new())
            }
            Request::Recommended { id } => {
                let (m, view) = find(id)?;
                if let Some(reason) = &view.blocked {
                    return Err(reason.clone());
                }
                if view.state != State::Foreign {
                    return Err(format!("{} no está instalado por tu cuenta.", m.name));
                }
                prefs.insert(
                    id.clone(),
                    ModPref {
                        enabled: true,
                        variant: Some(view.desired.variant.clone()),
                    },
                );
                let mut deps = with_deps(id);
                deps.remove(id);
                let install = Work::Install {
                    id: id.clone(),
                    variant: view.desired.variant.clone(),
                };
                (Some(deps), vec![install])
            }
            Request::Conflicts { id } => {
                let (m, view) = find(id)?;
                if view.record.is_none() || view.conflicts.is_empty() {
                    return Err(format!("{} no tiene nada que desactivar.", m.name));
                }
                (
                    Some(HashSet::new()),
                    vec![Work::Conflicts { id: id.clone() }],
                )
            }
        };
        if prefs != self.store.get().mods_prefs {
            self.store.update(|d| d.mods_prefs = prefs);
        }
        Ok(chosen)
    }

    /// Runs `request` against the game folder.
    pub async fn run(&self, catalog: &Arc<Catalog>, request: Request) -> ModsSyncResult {
        let Some(_busy) = self.state.begin() else {
            return ModsSyncResult::refused("Ya se están instalando mods.");
        };
        if (self.game_running)() {
            return ModsSyncResult::refused("Cierra GTA San Andreas antes de cambiar los mods.");
        }
        let survey = self.survey(catalog).await;
        let (only, extra) = match self.choose(catalog, &survey, &request) {
            Ok(chosen) => chosen,
            Err(message) => return ModsSyncResult::refused(message),
        };
        let Some(dir) = self.dir.clone() else {
            // Only the choices change until there is a game folder.
            return match request {
                Request::Recommended { .. } | Request::Conflicts { .. } => {
                    ModsSyncResult::refused("Elige primero la carpeta de GTA San Andreas.")
                }
                _ => ModsSyncResult::ok(),
            };
        };
        // What this request is about gets a fresh try.
        let skip: HashSet<String> = {
            let mut errors = lock(&self.state.errors);
            match (&request, &only) {
                (Request::Auto { retry: true }, _) => {
                    errors.clear();
                    HashSet::new()
                }
                (Request::Auto { retry: false }, _) => errors.keys().cloned().collect(),
                (_, Some(only)) => {
                    errors
                        .retain(|id, _| !only.contains(id) && !extra.iter().any(|w| w.id() == id));
                    HashSet::new()
                }
                (_, None) => HashSet::new(),
            }
        };
        // The choices may have changed: look again.
        let survey = self.survey(catalog).await;
        let mut works = status::work(catalog, &survey, only.as_ref(), &skip);
        works.extend(extra);
        if works.is_empty() {
            return ModsSyncResult::ok();
        }
        log::info!("mods: running {works:?} in {}", dir.display());

        let cancel = Cancel::default();
        *lock(&self.state.running) = Some(cancel.clone());
        lock(&self.state.installing).extend(works.iter().map(|w| w.id().to_string()));
        let job = Job {
            client: self.client.clone(),
            backups: self.backups_dir(&dir),
            dir,
            temp: self.temp.clone(),
            limits: self.limits,
            cancel: cancel.clone(),
            report: self.report.clone(),
            hasher: self.state.hasher.clone(),
            writable: self.writable.clone(),
            uac: self.uac,
        };
        let executed = install::execute(&job, catalog, &works, self.records()).await;
        self.save_records(executed.records);
        // An optional mod installed by default now counts as the user's
        // choice: a later change of the catalog's default never flips it.
        let prefs = self.store.get().mods_prefs;
        let chosen: Vec<(String, ModPref)> = works
            .iter()
            .filter_map(|work| match work {
                Work::Install { id, variant } if executed.done.contains(id) => Some((id, variant)),
                _ => None,
            })
            .filter(|(id, _)| {
                catalog.get(id).is_some_and(|m| !m.required) && !prefs.contains_key(*id)
            })
            .map(|(id, variant)| {
                let pref = ModPref {
                    enabled: true,
                    variant: Some(variant.clone()),
                };
                (id.clone(), pref)
            })
            .collect();
        if !chosen.is_empty() {
            self.store.update(|d| d.mods_prefs.extend(chosen));
        }
        {
            let mut errors = lock(&self.state.errors);
            for id in &executed.done {
                errors.remove(id);
            }
            for (id, error) in &executed.failed {
                errors.insert(id.clone(), error.clone());
            }
        }
        let failed: Vec<ModFailure> = executed
            .failed
            .iter()
            .map(|(id, error)| ModFailure {
                id: id.clone(),
                name: name_of(catalog, id),
                error: error.clone(),
            })
            .collect();
        for f in &failed {
            log::warn!("mods: {} failed: {}", f.id, f.error);
        }
        let done = Progress::new(
            if failed.is_empty() {
                Phase::Done
            } else {
                Phase::Error
            },
            100.0,
        );
        (self.report)(None, done);
        ModsSyncResult {
            ok: failed.is_empty() && !executed.cancelled,
            error: None,
            failed,
            cancelled: executed.cancelled.then_some(true),
        }
    }
}

// ---- the app side ----

fn engine(app: &AppHandle) -> Engine<'_> {
    let ctx = app.state::<Ctx>().inner();
    let event_app = app.clone();
    let report: ModReporter = Arc::new(move |id, progress| {
        events::emit(
            &event_app,
            MODS_INSTALL_PROGRESS,
            ModsProgress {
                id: id.map(String::from),
                progress,
            },
        )
    });
    let backups = app
        .path()
        .app_local_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("es.sarp.launcher"))
        .join("mod-backups");
    Engine {
        store: &ctx.store,
        state: &ctx.mods,
        dir: game_dir(&ctx.store),
        client: ctx.api.client().clone(),
        backups,
        temp: std::env::temp_dir(),
        limits: Limits::RESUMABLE,
        report,
        writable: Arc::new(|dir: &Path| can_write_to_dir(dir)),
        uac: true,
        launcher: app.package_info().version.clone(),
        game_running: Arc::new(|| find_game_process().is_some()),
    }
}

/// The Mods row and panel. `force`: fetch the catalog again.
pub async fn check(app: &AppHandle, force: bool) -> ModsHealth {
    let ctx = app.state::<Ctx>();
    let loaded = ctx.mods.loaded(&ctx.api, &ctx.store, force).await;
    engine(app).check(&loaded).await
}

pub async fn run(app: &AppHandle, request: Request) -> ModsSyncResult {
    let ctx = app.state::<Ctx>();
    let loaded = ctx.mods.loaded(&ctx.api, &ctx.store, false).await;
    match &loaded.catalog {
        Some(catalog) => engine(app).run(catalog, request).await,
        None => ModsSyncResult::refused("No hay una lista de mods verificada."),
    }
}

pub fn cancel(app: &AppHandle) -> bool {
    let cancelled = app.state::<Ctx>().mods.cancel();
    log::info!("mods run cancel requested (running: {cancelled})");
    cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_in_spanish() {
        assert_eq!(names(&[]), "");
        assert_eq!(names(&["A".into()]), "A");
        assert_eq!(names(&["A".into(), "B".into()]), "A y B");
        assert_eq!(names(&["A".into(), "B".into(), "C".into()]), "A, B y C");
    }

    #[test]
    fn catalog_messages() {
        let loaded = |origin, problem| Loaded {
            catalog: None,
            origin,
            problem,
        };
        assert_eq!(catalog_info(&loaded(Origin::Live, None)).detail, None);
        assert_eq!(
            catalog_info(&loaded(Origin::Empty, None)).state,
            CatalogState::Empty
        );
        assert!(
            catalog_info(&loaded(Origin::Missing, Some(Problem::BadSignature)))
                .detail
                .unwrap()
                .contains("no se instaló nada")
        );
        assert!(
            catalog_info(&loaded(Origin::Saved, Some(Problem::Rollback)))
                .detail
                .unwrap()
                .contains("última verificada")
        );
        assert_eq!(
            serde_json::to_value(catalog_info(&loaded(
                Origin::Missing,
                Some(Problem::Unreachable)
            )))
            .unwrap(),
            json!({
                "state": "missing",
                "detail": "No se pudo obtener la lista de mods. Revisa tu conexión a Internet."
            })
        );
    }

    #[test]
    fn progress_payload_is_flat() {
        let progress = ModsProgress {
            id: Some("radar".into()),
            progress: Progress::new(Phase::Download, 50.0)
                .bytes(5, 10)
                .message("Descargando Radar..."),
        };
        assert_eq!(
            serde_json::to_value(progress).unwrap(),
            json!({
                "id": "radar", "phase": "download", "percent": 50.0,
                "bytesDone": 5, "bytesTotal": 10, "message": "Descargando Radar..."
            })
        );
    }
}
