//! What each mod looks like in the game folder and what should happen to it.
//! Pure over its inputs plus read-only looks at the folder (blocking: it
//! hashes files), so the same survey drives the health check, the automatic
//! flow and the panel's actions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use semver::Version;
use serde::Serialize;

use super::catalog::{Catalog, Mod, Variant};
use super::disk::{exists, Hasher};
use super::record::{ModPref, ModRecord, RecordedFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    NotInstalled,
    Installed,
    /// Installed by us, and the catalog has another version (or files).
    Outdated,
    /// Installed by us, and a file is missing or was modified.
    Damaged,
    /// Not installed by us, but in place by other means (`equivalents`, or
    /// our files while the user turned the mod off): left alone.
    Foreign,
    /// Can't be installed: launcher too old, or a dependency unavailable.
    Blocked,
}

/// What the automatic flow does with a mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    None,
    Install,
    Update,
    Repair,
    /// Another variant is wanted.
    Switch,
    /// Turned off: our files go.
    Remove,
}

/// What the user (or the catalog's defaults) wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    pub enabled: bool,
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub id: String,
    pub state: State,
    pub desired: Desired,
    pub action: Action,
    pub blocked: Option<String>,
    /// The record after adoption, if the mod is ours.
    pub record: Option<ModRecord>,
    /// `replaces` paths present in the folder (offered to disable while the
    /// mod is ours).
    pub conflicts: Vec<String>,
}

pub struct Inputs<'a> {
    pub catalog: &'a Catalog,
    /// `None`: no valid game folder; nothing on disk is looked at.
    pub dir: Option<PathBuf>,
    pub records: &'a BTreeMap<String, ModRecord>,
    pub prefs: &'a BTreeMap<String, ModPref>,
    pub launcher: &'a Version,
}

#[derive(Debug, Default)]
pub struct Survey {
    /// In catalog order.
    pub views: Vec<View>,
    /// Records to save: mods found in place with our exact files.
    pub adopted: Vec<(String, ModRecord)>,
    /// Preferences to save along with them.
    pub prefs: Vec<(String, ModPref)>,
}

impl Survey {
    pub fn view(&self, id: &str) -> Option<&View> {
        self.views.iter().find(|v| v.id == id)
    }
}

/// Mods that can't be installed, with the reason: a launcher older than
/// `minLauncher`, or a dependency that is missing from the catalog or
/// blocked itself.
pub fn blocked(catalog: &Catalog, launcher: &Version) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for m in &catalog.mods {
        if let Some(min) = m.min_launcher.as_ref().filter(|min| launcher < *min) {
            out.insert(
                m.id.clone(),
                format!("Necesita la versión {min} del launcher o una más nueva."),
            );
        }
    }
    loop {
        let mut changed = false;
        for m in &catalog.mods {
            if out.contains_key(&m.id) {
                continue;
            }
            for dep in &m.requires {
                let reason = match catalog.get(dep) {
                    None => Some(format!("Necesita «{dep}», que no está disponible.")),
                    Some(d) if out.contains_key(&d.id) => {
                        Some(format!("Necesita {}, que no se puede instalar.", d.name))
                    }
                    Some(_) => None,
                };
                if let Some(reason) = reason {
                    out.insert(m.id.clone(), reason);
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return out;
        }
    }
}

/// Wanted state per mod: required mods always on; optional ones as the user
/// chose, else the catalog's default. An enabled mod switches its
/// dependencies on too.
pub fn desired(catalog: &Catalog, prefs: &BTreeMap<String, ModPref>) -> HashMap<String, Desired> {
    let mut out: HashMap<String, Desired> = catalog
        .mods
        .iter()
        .map(|m| {
            let pref = prefs.get(&m.id);
            let variant = pref
                .and_then(|p| p.variant.as_deref())
                .filter(|v| m.variant(v).is_some())
                .unwrap_or(&m.default_variant)
                .to_string();
            let enabled = m.required || pref.map_or(m.enabled_by_default, |p| p.enabled);
            (m.id.clone(), Desired { enabled, variant })
        })
        .collect();
    loop {
        let mut changed = false;
        for m in &catalog.mods {
            if !out[&m.id].enabled {
                continue;
            }
            for dep in &m.requires {
                if let Some(d) = out.get_mut(dep).filter(|d| !d.enabled) {
                    d.enabled = true;
                    changed = true;
                }
            }
        }
        if !changed {
            return out;
        }
    }
}

/// Catalog indexes, every mod after the ones it requires.
pub fn order(catalog: &Catalog) -> Vec<usize> {
    fn visit(i: usize, catalog: &Catalog, done: &mut Vec<bool>, out: &mut Vec<usize>) {
        if done[i] {
            return;
        }
        done[i] = true;
        for dep in &catalog.mods[i].requires {
            if let Some(j) = catalog.mods.iter().position(|m| &m.id == dep) {
                visit(j, catalog, done, out);
            }
        }
        out.push(i);
    }
    let mut done = vec![false; catalog.mods.len()];
    let mut out = Vec::with_capacity(catalog.mods.len());
    for i in 0..catalog.mods.len() {
        visit(i, catalog, &mut done, &mut out);
    }
    out
}

/// Every mod `id` needs, directly or not.
pub fn dependencies(catalog: &Catalog, id: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(next) = stack.pop() {
        if let Some(m) = catalog.get(&next) {
            for dep in &m.requires {
                if out.insert(dep.clone()) {
                    stack.push(dep.clone());
                }
            }
        }
    }
    out
}

/// Enabled mods that need `id` (they lock it on).
pub fn dependents<'c>(
    catalog: &'c Catalog,
    wanted: &HashMap<String, Desired>,
    id: &str,
) -> Vec<&'c Mod> {
    catalog
        .mods
        .iter()
        .filter(|m| {
            m.id != id && wanted[&m.id].enabled && dependencies(catalog, &m.id).contains(id)
        })
        .collect()
}

fn files_key(files: impl Iterator<Item = (String, u64, String)>) -> Vec<(String, u64, String)> {
    let mut key: Vec<_> = files.collect();
    key.sort();
    key
}

/// The recorded files (not `keep`) are those of `variant`.
fn same_files(record: &ModRecord, variant: &Variant) -> bool {
    let recorded = files_key(record.files.iter().filter(|f| !f.keep).map(|f| {
        (
            f.path.to_ascii_lowercase(),
            f.bytes,
            f.sha256.to_ascii_lowercase(),
        )
    }));
    let listed = files_key(
        variant
            .files
            .iter()
            .filter(|f| !f.keep)
            .map(|f| (f.path.to_ascii_lowercase(), f.bytes, f.sha256.clone())),
    );
    recorded == listed
}

/// A variant whose files are all in place (`keep` ones aside): what a
/// record would say if we had installed it.
fn adoptable<'m>(m: &'m Mod, dir: &std::path::Path, hasher: &Hasher) -> Option<&'m Variant> {
    let default = m.default_variant();
    std::iter::once(default)
        .chain(m.variants.iter().filter(|v| v.id != default.id))
        .find(|v| {
            let mut own = v.files.iter().filter(|f| !f.keep).peekable();
            own.peek().is_some() && own.all(|f| hasher.matches(dir, &f.path, f.bytes, &f.sha256))
        })
}

fn adopt(variant: &Variant, dir: &std::path::Path) -> ModRecord {
    ModRecord {
        variant: variant.id.clone(),
        version: variant.version.clone(),
        files: variant
            .files
            .iter()
            .filter(|f| !f.keep || exists(dir, &f.path))
            .map(|f| RecordedFile {
                path: f.path.clone(),
                bytes: f.bytes,
                sha256: f.sha256.clone(),
                keep: f.keep,
                backup: false,
            })
            .collect(),
        disabled: Vec::new(),
        adopted: true,
    }
}

/// Looks at every mod of the catalog. Blocking.
pub fn survey(inputs: &Inputs<'_>, hasher: &Hasher) -> Survey {
    let catalog = inputs.catalog;
    let blocked = blocked(catalog, inputs.launcher);
    let wanted = desired(catalog, inputs.prefs);
    let mut survey = Survey::default();
    for m in &catalog.mods {
        let desired = wanted[&m.id].clone();
        let blocked = blocked.get(&m.id).cloned();
        let mut record = inputs.records.get(&m.id).cloned();
        let state = match (&inputs.dir, &record) {
            (None, _) => State::NotInstalled,
            (Some(dir), Some(rec)) => {
                let damaged = rec
                    .files
                    .iter()
                    .filter(|f| !f.keep)
                    .any(|f| !hasher.matches(dir, &f.path, f.bytes, &f.sha256));
                let current = m
                    .variant(&rec.variant)
                    .is_some_and(|v| v.version == rec.version && same_files(rec, v));
                if damaged {
                    State::Damaged
                } else if current {
                    State::Installed
                } else {
                    State::Outdated
                }
            }
            (Some(dir), None) => {
                let turned_off = inputs.prefs.get(&m.id).is_some_and(|p| !p.enabled) && !m.required;
                if let Some(variant) = adoptable(m, dir, hasher) {
                    if turned_off {
                        // Our files, but the user turned the mod off: theirs.
                        State::Foreign
                    } else {
                        let adopted = adopt(variant, dir);
                        log::info!("mods: adopted {} {} found in place", m.id, variant.id);
                        survey.adopted.push((m.id.clone(), adopted.clone()));
                        if !inputs.prefs.contains_key(&m.id) {
                            survey.prefs.push((
                                m.id.clone(),
                                ModPref {
                                    enabled: true,
                                    variant: Some(variant.id.clone()),
                                },
                            ));
                        }
                        record = Some(adopted);
                        State::Installed
                    }
                } else if m
                    .equivalents
                    .iter()
                    .any(|set| set.iter().all(|path| exists(dir, path)))
                {
                    State::Foreign
                } else {
                    State::NotInstalled
                }
            }
        };
        // An adopted variant is what the user has: keep it unless they chose.
        let desired = match (&record, inputs.prefs.get(&m.id)) {
            (Some(rec), None) if rec.adopted && m.variant(&rec.variant).is_some() => Desired {
                variant: rec.variant.clone(),
                ..desired
            },
            _ => desired,
        };
        let state = if blocked.is_some() && record.is_none() && state != State::Foreign {
            State::Blocked
        } else {
            state
        };
        let action = match (&blocked, desired.enabled, state, &record) {
            // Removing never needs the catalog's newer version.
            (_, false, _, Some(_)) => Action::Remove,
            (Some(_), ..) | (None, _, State::Blocked, _) => Action::None,
            (None, false, _, None) => Action::None,
            (None, true, State::Foreign, _) => Action::None,
            (None, true, State::NotInstalled, _) => Action::Install,
            (None, true, State::Outdated, _) => Action::Update,
            (None, true, State::Damaged, _) => Action::Repair,
            (None, true, State::Installed, Some(rec)) if rec.variant != desired.variant => {
                Action::Switch
            }
            (None, true, State::Installed, _) => Action::None,
        };
        let conflicts = match (&inputs.dir, &record) {
            (Some(dir), Some(_)) => m
                .replaces
                .iter()
                .filter(|path| exists(dir, path))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        survey.views.push(View {
            id: m.id.clone(),
            state,
            desired,
            action,
            blocked,
            record,
            conflicts,
        });
    }
    survey
}

/// One step of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    Install {
        id: String,
        variant: String,
    },
    Remove {
        id: String,
    },
    /// Rename the mod's present `replaces` out of the way.
    Conflicts {
        id: String,
    },
}

impl Work {
    pub fn id(&self) -> &str {
        match self {
            Self::Install { id, .. } | Self::Remove { id } | Self::Conflicts { id } => id,
        }
    }
}

/// The work for the mods in `only` (all when `None`) that have an action:
/// removals first (dependents before what they need), then installs (what
/// is needed first). `skip`: mods to leave for now (failed this session).
pub fn work(
    catalog: &Catalog,
    survey: &Survey,
    only: Option<&HashSet<String>>,
    skip: &HashSet<String>,
) -> Vec<Work> {
    let order = order(catalog);
    let picked = |i: &usize| {
        let id = &catalog.mods[*i].id;
        only.is_none_or(|only| only.contains(id)) && !skip.contains(id)
    };
    let mut out = Vec::new();
    for i in order.iter().rev().filter(|i| picked(i)) {
        let view = &survey.views[*i];
        if view.action == Action::Remove {
            out.push(Work::Remove {
                id: view.id.clone(),
            });
        }
    }
    for i in order.iter().filter(|i| picked(i)) {
        let view = &survey.views[*i];
        if matches!(
            view.action,
            Action::Install | Action::Update | Action::Repair | Action::Switch
        ) {
            out.push(Work::Install {
                id: view.id.clone(),
                variant: view.desired.variant.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mods::catalog::parse;
    use crate::mods::catalog::tests::{a_mod, variant};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::fs;

    pub fn sha(body: &[u8]) -> String {
        hex::encode(Sha256::digest(body))
    }

    fn file(path: &str, body: &[u8]) -> Value {
        json!({ "path": path, "bytes": body.len(), "sha256": sha(body) })
    }

    fn catalog(mods: Vec<Value>) -> Catalog {
        parse(
            &json!({ "schemaVersion": 1, "serial": 1, "mods": mods }).to_string(),
            true,
        )
        .unwrap()
    }

    fn with(id: &str, change: impl FnOnce(&mut Value)) -> Value {
        let mut m = a_mod(id);
        change(&mut m);
        m
    }

    fn launcher() -> Version {
        Version::new(1, 2, 0)
    }

    fn run(
        catalog: &Catalog,
        dir: Option<&std::path::Path>,
        records: &BTreeMap<String, ModRecord>,
        prefs: &BTreeMap<String, ModPref>,
    ) -> Survey {
        survey(
            &Inputs {
                catalog,
                dir: dir.map(|d| d.to_path_buf()),
                records,
                prefs,
                launcher: &launcher(),
            },
            &Hasher::default(),
        )
    }

    fn pref(enabled: bool, variant: Option<&str>) -> ModPref {
        ModPref {
            enabled,
            variant: variant.map(String::from),
        }
    }

    #[test]
    fn launcher_version_and_dependencies_block() {
        let c = catalog(vec![
            with("new", |m| m["minLauncher"] = json!("1.3.0")),
            with("same", |m| m["minLauncher"] = json!("1.2.0")),
            with("needs-new", |m| m["requires"] = json!(["new"])),
            with("needs-ghost", |m| m["requires"] = json!(["ghost"])),
            with("chain", |m| m["requires"] = json!(["needs-ghost"])),
            a_mod("free"),
        ]);
        let blocked = blocked(&c, &launcher());
        let mut ids: Vec<_> = blocked.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["chain", "needs-ghost", "needs-new", "new"]);
        assert_eq!(
            blocked["new"],
            "Necesita la versión 1.3.0 del launcher o una más nueva."
        );
        assert_eq!(
            blocked["needs-ghost"],
            "Necesita «ghost», que no está disponible."
        );
        assert_eq!(
            blocked["chain"],
            "Necesita needs-ghost, que no se puede instalar."
        );
        let survey = run(&c, None, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(survey.view("new").unwrap().state, State::Blocked);
        assert_eq!(survey.view("free").unwrap().state, State::NotInstalled);
    }

    #[test]
    fn defaults_prefs_and_dependencies_decide_what_is_wanted() {
        let c = catalog(vec![
            with("base", |m| m["required"] = json!(true)),
            with("lib", |m| m["enabledByDefault"] = json!(false)),
            with("radar", |m| {
                m["enabledByDefault"] = json!(true);
                m["requires"] = json!(["lib", "base"]);
                let files = m["variants"][0]["files"].clone();
                m["variants"] = json!([variant("default", files.clone()), variant("hd", files)]);
            }),
            with("off", |m| m["enabledByDefault"] = json!(false)),
        ]);
        let wanted = desired(&c, &BTreeMap::new());
        assert!(wanted["base"].enabled && wanted["radar"].enabled);
        assert!(wanted["lib"].enabled, "pulled in by radar");
        assert!(!wanted["off"].enabled);
        assert_eq!(wanted["radar"].variant, "default");

        let prefs = BTreeMap::from([
            ("base".to_string(), pref(false, None)),
            ("radar".to_string(), pref(true, Some("hd"))),
            ("off".to_string(), pref(true, Some("gone"))),
        ]);
        let wanted = desired(&c, &prefs);
        assert!(wanted["base"].enabled, "required mods can't be turned off");
        assert_eq!(wanted["radar"].variant, "hd");
        assert!(wanted["off"].enabled);
        assert_eq!(
            wanted["off"].variant, "default",
            "unknown variant: the default"
        );

        let prefs = BTreeMap::from([("radar".to_string(), pref(false, None))]);
        let wanted = desired(&c, &prefs);
        assert!(!wanted["radar"].enabled && !wanted["lib"].enabled);
        assert_eq!(
            dependents(&c, &desired(&c, &BTreeMap::new()), "lib")
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["radar"]
        );
        assert!(dependents(&c, &wanted, "lib").is_empty());
        let mut deps: Vec<_> = dependencies(&c, "radar").into_iter().collect();
        deps.sort();
        assert_eq!(deps, ["base", "lib"]);
    }

    #[test]
    fn installs_go_in_dependency_order_and_removals_in_reverse() {
        let c = catalog(vec![
            with("radar", |m| m["requires"] = json!(["modloader"])),
            with("skin", |m| m["requires"] = json!(["radar"])),
            a_mod("modloader"),
        ]);
        let order: Vec<&str> = order(&c)
            .into_iter()
            .map(|i| c.mods[i].id.as_str())
            .collect();
        assert_eq!(order, ["modloader", "radar", "skin"]);
        let prefs: BTreeMap<String, ModPref> = c
            .mods
            .iter()
            .map(|m| (m.id.clone(), pref(true, None)))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let survey = run(&c, Some(dir.path()), &BTreeMap::new(), &prefs);
        let all = work(&c, &survey, None, &HashSet::new());
        assert_eq!(
            all.iter().map(Work::id).collect::<Vec<_>>(),
            ["modloader", "radar", "skin"]
        );
        let skip = HashSet::from(["radar".to_string()]);
        assert_eq!(work(&c, &survey, None, &skip).len(), 2);
        let only = HashSet::from(["skin".to_string()]);
        assert_eq!(
            work(&c, &survey, Some(&only), &HashSet::new()),
            [Work::Install {
                id: "skin".into(),
                variant: "default".into()
            }]
        );

        // Everything installed, everything turned off: dependents go first.
        let records: BTreeMap<String, ModRecord> = c
            .mods
            .iter()
            .map(|m| {
                let v = m.default_variant();
                (m.id.clone(), adopt(v, dir.path()))
            })
            .collect();
        let off: BTreeMap<String, ModPref> = c
            .mods
            .iter()
            .map(|m| (m.id.clone(), pref(false, None)))
            .collect();
        let survey = run(&c, Some(dir.path()), &records, &off);
        let removals = work(&c, &survey, None, &HashSet::new());
        assert_eq!(
            removals,
            ["skin", "radar", "modloader"].map(|id| Work::Remove { id: id.into() })
        );
    }

    #[test]
    fn states_from_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (b"one".as_slice(), b"two".as_slice());
        let c = catalog(vec![with("x", |m| {
            m["variants"] = json!([
                variant(
                    "default",
                    json!([file("x.asi", one), {
                        "path": "x.ini", "bytes": 3, "sha256": sha(b"cfg"), "keep": true
                    }])
                ),
                variant("hd", json!([file("x.asi", two)])),
            ]);
            m["enabledByDefault"] = json!(true);
            m["equivalents"] = json!([["other.asi", "otherdir/"]]);
        })]);
        let none = BTreeMap::new();
        let no_prefs = BTreeMap::new();
        let state = |records: &BTreeMap<String, ModRecord>, prefs: &BTreeMap<String, ModPref>| {
            let survey = run(&c, Some(dir.path()), records, prefs);
            let view = survey.view("x").unwrap().clone();
            (view.state, view.action, survey)
        };
        assert_eq!(state(&none, &no_prefs).0, State::NotInstalled);
        assert_eq!(state(&none, &no_prefs).1, Action::Install);

        // Equivalents: all paths of a set, folders as folders.
        fs::write(dir.path().join("other.asi"), b"o").unwrap();
        assert_eq!(state(&none, &no_prefs).0, State::NotInstalled);
        fs::write(dir.path().join("otherdir"), b"a file, not a folder").unwrap();
        assert_eq!(state(&none, &no_prefs).0, State::NotInstalled);
        fs::remove_file(dir.path().join("otherdir")).unwrap();
        fs::create_dir(dir.path().join("otherdir")).unwrap();
        let (st, action, _) = state(&none, &no_prefs);
        assert_eq!((st, action), (State::Foreign, Action::None));

        // Our exact files (the HD ones), unrecorded: adopted, with the user's
        // variant kept as their choice.
        fs::write(dir.path().join("x.asi"), two).unwrap();
        let (st, action, survey) = state(&none, &no_prefs);
        assert_eq!((st, action), (State::Installed, Action::None));
        assert_eq!(survey.adopted.len(), 1);
        let adopted = survey.adopted[0].1.clone();
        assert!(adopted.adopted && adopted.variant == "hd");
        assert_eq!(survey.prefs, [("x".to_string(), pref(true, Some("hd")))]);
        // Turned off by the user: not adopted, left alone.
        let off = BTreeMap::from([("x".to_string(), pref(false, None))]);
        let (st, action, survey) = state(&none, &off);
        assert_eq!((st, action), (State::Foreign, Action::None));
        assert!(survey.adopted.is_empty());

        // Recorded: installed, wanted elsewhere, damaged, outdated.
        let records = BTreeMap::from([("x".to_string(), adopted.clone())]);
        let hd = BTreeMap::from([("x".to_string(), pref(true, Some("hd")))]);
        assert_eq!(state(&records, &hd).0, State::Installed);
        let lq = BTreeMap::from([("x".to_string(), pref(true, Some("default")))]);
        assert_eq!(state(&records, &lq).1, Action::Switch);
        assert_eq!(state(&records, &off).1, Action::Remove);
        fs::write(dir.path().join("x.asi"), b"tampered").unwrap();
        assert_eq!(state(&records, &hd).0, State::Damaged);
        assert_eq!(state(&records, &hd).1, Action::Repair);
        fs::remove_file(dir.path().join("x.asi")).unwrap();
        assert_eq!(state(&records, &hd).0, State::Damaged);
        fs::write(dir.path().join("x.asi"), two).unwrap();
        let mut old = adopted;
        old.version = "0.9".into();
        let records = BTreeMap::from([("x".to_string(), old)]);
        let (st, action, _) = state(&records, &hd);
        assert_eq!((st, action), (State::Outdated, Action::Update));
        // No folder: nothing is looked at.
        let survey = run(&c, None, &records, &hd);
        assert_eq!(survey.view("x").unwrap().state, State::NotInstalled);
    }

    #[test]
    fn keep_files_never_count_against_a_mod() {
        let dir = tempfile::tempdir().unwrap();
        let c = catalog(vec![with("x", |m| {
            m["variants"] = json!([variant(
                "default",
                json!([file("x.asi", b"x"), {
                    "path": "x.ini", "bytes": 3, "sha256": sha(b"cfg"), "keep": true
                }])
            )]);
        })]);
        fs::write(dir.path().join("x.asi"), b"x").unwrap();
        fs::write(dir.path().join("x.ini"), b"the user's own settings").unwrap();
        let prefs = BTreeMap::from([("x".to_string(), pref(true, None))]);
        let survey = run(&c, Some(dir.path()), &BTreeMap::new(), &prefs);
        assert_eq!(survey.view("x").unwrap().state, State::Installed);
        let record = survey.adopted[0].1.clone();
        assert!(record.owns("x.ini").is_some_and(|f| f.keep));
        let records = BTreeMap::from([("x".to_string(), record)]);
        fs::remove_file(dir.path().join("x.ini")).unwrap();
        let survey = run(&c, Some(dir.path()), &records, &prefs);
        assert_eq!(survey.view("x").unwrap().state, State::Installed);
    }

    #[test]
    fn conflicts_are_offered_only_for_our_mods() {
        let dir = tempfile::tempdir().unwrap();
        let c = catalog(vec![with("radar", |m| {
            m["replaces"] = json!(["modloader/radar_hq", "modloader/radar_lq"]);
            m["variants"] = json!([variant("default", json!([file("radar.txd", b"r")]))]);
            m["enabledByDefault"] = json!(true);
        })]);
        fs::create_dir_all(dir.path().join("modloader").join("radar_hq")).unwrap();
        let survey = run(&c, Some(dir.path()), &BTreeMap::new(), &BTreeMap::new());
        assert!(survey.view("radar").unwrap().conflicts.is_empty());
        fs::write(dir.path().join("radar.txd"), b"r").unwrap();
        let survey = run(&c, Some(dir.path()), &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(
            survey.view("radar").unwrap().conflicts,
            ["modloader/radar_hq"]
        );
    }
}
