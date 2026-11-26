//! The mods engine end to end against a local HTTP server
//! (`net::test_server`), always on temp folders: fake game folders, a temp
//! store, temp backups and downloads. Elevation is never requested: the
//! "protected folder" case runs the same script without UAC.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use semver::Version;
use serde_json::{json, Value};

use super::catalog::{parse, Catalog};
use super::install::PACKAGES;
use super::record::{folder_key, ModRecord};
use super::source::{Loaded, Origin};
use super::status::{Action, State};
use super::*;
use crate::game::paths::tests::fake_install;
use crate::install::cache::package::tests::build_zip;
use crate::install::progress::Progress;
use crate::net::test_server::{Fault, TestServer};

fn sha(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// A file of a variant.
struct F {
    path: &'static str,
    body: Vec<u8>,
    keep: bool,
}

fn f(path: &'static str, body: &[u8]) -> F {
    F {
        path,
        body: body.to_vec(),
        keep: false,
    }
}

fn keep(path: &'static str, body: &[u8]) -> F {
    F {
        keep: true,
        ..f(path, body)
    }
}

/// Progress reported, per mod (`None`: the whole run).
type Events = Arc<Mutex<Vec<(Option<String>, Progress)>>>;

struct Fx {
    server: TestServer,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    game: PathBuf,
    store: Store,
    state: ModsState,
    backups: PathBuf,
    temp: PathBuf,
    events: Events,
    running: Arc<AtomicBool>,
    writable: Arc<AtomicBool>,
}

impl Fx {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let game = root.join("GTA San Andreas");
        fs::create_dir_all(&game).unwrap();
        fake_install(&game);
        fs::write(
            game.join("vorbisFile.dll"),
            b"the game's own vorbisFile.dll",
        )
        .unwrap();
        let temp = root.join("temp");
        fs::create_dir_all(&temp).unwrap();
        Self {
            server: TestServer::start().await,
            store: Store::open(root.join("store.json")),
            state: ModsState::default(),
            backups: root.join("mod-backups"),
            temp,
            game,
            root,
            _tmp: tmp,
            events: Arc::default(),
            running: Arc::default(),
            writable: Arc::new(AtomicBool::new(true)),
        }
    }

    fn engine(&self) -> Engine<'_> {
        let sink = self.events.clone();
        let (writable, running) = (self.writable.clone(), self.running.clone());
        Engine {
            store: &self.store,
            state: &self.state,
            dir: Some(self.game.clone()),
            client: crate::net::http::strict_client("test"),
            backups: self.backups.clone(),
            temp: self.temp.clone(),
            limits: crate::net::http::tests::fast(),
            report: Arc::new(move |id, p| sink.lock().unwrap().push((id.map(String::from), p))),
            writable: Arc::new(move |_| writable.load(Ordering::SeqCst)),
            uac: false,
            launcher: Version::new(1, 2, 0),
            game_running: Arc::new(move || running.load(Ordering::SeqCst)),
        }
    }

    /// A variant whose package (with `extra` entries besides its files) is
    /// served by the test server.
    fn variant(
        &self,
        m: &str,
        id: &str,
        version: &str,
        target: &str,
        files: &[F],
        extra: &[(&str, &[u8])],
    ) -> Value {
        let mut entries: Vec<(&str, &[u8])> =
            files.iter().map(|f| (f.path, f.body.as_slice())).collect();
        entries.extend_from_slice(extra);
        let zip = build_zip(&entries);
        let path = format!("/pkg/{m}/{id}-{version}.zip");
        let value = json!({
            "id": id, "label": format!("Variante {id}"), "version": version, "target": target,
            "package": { "url": format!("{}{path}", self.server.base), "bytes": zip.len(), "sha256": sha(&zip) },
            "files": files.iter().map(|f| json!({
                "path": f.path, "bytes": f.body.len(), "sha256": sha(&f.body), "keep": f.keep
            })).collect::<Vec<_>>()
        });
        self.server.put(&path, zip);
        value
    }

    fn catalog(&self, mods: Vec<Value>) -> Arc<Catalog> {
        let text = json!({ "schemaVersion": 1, "serial": 1, "mods": mods }).to_string();
        Arc::new(parse(&text, false).unwrap())
    }

    async fn check(&self, catalog: &Arc<Catalog>) -> ModsHealth {
        let loaded = Loaded {
            catalog: Some(catalog.clone()),
            origin: Origin::Live,
            problem: None,
        };
        self.engine().check(&loaded).await
    }

    async fn run(&self, catalog: &Arc<Catalog>, request: Request) -> ModsSyncResult {
        self.engine().run(catalog, request).await
    }

    async fn auto(&self, catalog: &Arc<Catalog>) -> ModsSyncResult {
        self.run(catalog, Request::Auto { retry: false }).await
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        fs::read(self.game.join(rel)).ok()
    }

    fn records(&self) -> BTreeMap<String, ModRecord> {
        self.store
            .get()
            .mods_installed
            .get(&folder_key(&self.game))
            .map(|i| i.0.clone())
            .unwrap_or_default()
    }

    /// Every file under `dir`, relative, sorted.
    fn tree(dir: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if entry.file_type().unwrap().is_dir() {
                    out.push(format!("{rel}/"));
                    walk(base, &path, out);
                } else {
                    out.push(rel);
                }
            }
        }
        let mut out = Vec::new();
        if dir.exists() {
            walk(dir, dir, &mut out);
        }
        out.sort();
        out
    }

    fn game_tree(&self) -> Vec<String> {
        Self::tree(&self.game)
    }

    fn assert_no_litter(&self) {
        let litter: Vec<String> = self
            .game_tree()
            .into_iter()
            .chain(Self::tree(&self.temp))
            .filter(|p| {
                p.ends_with(".sarp-part") || p.contains("sarp-launcher-") || p.contains(PACKAGES)
            })
            .collect();
        assert!(litter.is_empty(), "left behind: {litter:?}");
    }

    fn status<'h>(health: &'h ModsHealth, id: &str) -> &'h ModStatus {
        health.mods.iter().find(|m| m.id == id).unwrap()
    }
}

const ML_ASI: &[u8] = b"modloader.asi v1";
const ML_INI: &[u8] = b"[Config]\nEnableMenu=1\n";
const SILENT: &[u8] = b"Silent's ASI Loader as vorbisFile.dll";
const RADAR_LQ: &[u8] = b"radar, standard quality";
const RADAR_HD: &[u8] = b"radar, high definition!";

/// The three real mods: Modloader and an ASI loader (required), the radar
/// (optional, on by default, LQ/HD, replaces hand-made radars).
fn real_mods(fx: &Fx) -> Vec<Value> {
    vec![
        json!({
            "id": "modloader", "name": "Modloader", "description": "Carga mods.",
            "category": "essential", "required": true, "enabledByDefault": true,
            "equivalents": [["modloader.asi", "modloader/"]], "defaultVariant": "default",
            "variants": [fx.variant("modloader", "default", "0.3.7", "game",
                &[f("modloader.asi", ML_ASI), keep("modloader/modloader.ini", ML_INI)], &[])]
        }),
        json!({
            "id": "asi-loader", "name": "ASI Loader", "category": "essential",
            "required": true, "equivalents": [["dinput8.dll"]], "defaultVariant": "silent",
            "variants": [fx.variant("asi-loader", "silent", "1.3", "game", &[f("vorbisFile.dll", SILENT)], &[])]
        }),
        json!({
            "id": "radar", "name": "Radar", "category": "hud", "enabledByDefault": true,
            "requires": ["modloader"], "replaces": ["modloader/radar_hq", "modloader/radar_lq"],
            "defaultVariant": "lq",
            "variants": [
                fx.variant("radar", "lq", "2.0.0", "modloader",
                    &[f("radar_sarp/radar00.txd", RADAR_LQ), f("radar_sarp/lq-only.txd", b"lq")], &[]),
                fx.variant("radar", "hd", "2.0.0", "modloader", &[f("radar_sarp/radar00.txd", RADAR_HD)], &[])
            ]
        }),
    ]
}

#[tokio::test]
async fn a_fresh_folder_gets_required_mods_and_the_default_radar_by_itself() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    let health = fx.check(&catalog).await;
    assert!(health.pending && health.ready && !health.blocking);
    for id in ["modloader", "asi-loader", "radar"] {
        let status = Fx::status(&health, id);
        assert_eq!(
            (status.state, status.action),
            (State::NotInstalled, Action::Install),
            "{id}"
        );
    }
    assert_eq!(Fx::status(&health, "radar").requires, ["Modloader"]);
    assert_eq!(Fx::status(&health, "modloader").required_by, ["Radar"]);

    let result = fx.auto(&catalog).await;
    assert!(result.ok, "{result:?}");
    assert_eq!(fx.read("modloader.asi").unwrap(), ML_ASI);
    assert_eq!(fx.read("modloader/modloader.ini").unwrap(), ML_INI);
    assert_eq!(fx.read("vorbisFile.dll").unwrap(), SILENT);
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_LQ
    );
    // The game's own vorbisFile.dll went to the backups, for an uninstall.
    let backup = fx
        .backups
        .join(&hex::encode(Sha256::digest(folder_key(&fx.game).as_bytes()))[..16])
        .join("asi-loader")
        .join("vorbisFile.dll");
    assert_eq!(fs::read(&backup).unwrap(), b"the game's own vorbisFile.dll");
    let records = fx.records();
    assert!(records["asi-loader"].owns("vorbisFile.dll").unwrap().backup);
    assert!(
        records["modloader"]
            .owns("modloader/modloader.ini")
            .unwrap()
            .keep
    );
    assert_eq!(records["radar"].variant, "lq");
    // Installed by default: now the user's choice.
    assert_eq!(
        fx.store.get().mods_prefs.get("radar"),
        Some(&ModPref {
            enabled: true,
            variant: Some("lq".into())
        })
    );
    fx.assert_no_litter();
    // Packages only, never with the launcher key.
    for request in fx.server.requests() {
        assert!(request.path.starts_with("/pkg/"), "{}", request.path);
        assert!(!request.headers.contains_key("x-launcher-key"));
    }
    // Progress named each mod and ended with `done`.
    {
        let events = fx.events.lock().unwrap();
        assert!(events.iter().any(|(id, p)| id.as_deref() == Some("radar")
            && p.message.as_deref() == Some("Descargando Radar...")));
        assert_eq!(events.last().unwrap().1.phase, Phase::Done);
    }

    let health = fx.check(&catalog).await;
    assert!(!health.pending, "{health:?}");
    for m in &health.mods {
        assert_eq!(
            (m.state, m.action, m.error.as_deref()),
            (State::Installed, Action::None, None),
            "{}",
            m.id
        );
    }
    // Nothing to do: nothing downloaded again.
    fx.server.forget_requests();
    assert!(fx.auto(&catalog).await.ok);
    assert!(fx.server.requests().is_empty());
}

#[tokio::test]
async fn a_corrupt_package_fails_and_blocks_play_only_for_a_required_mod() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    fx.server
        .fault("/pkg/modloader/default-0.3.7.zip", Some(Fault::Corrupt));
    let result = fx.auto(&catalog).await;
    assert!(!result.ok);
    let failed: BTreeMap<_, _> = result
        .failed
        .iter()
        .map(|f| (f.id.as_str(), f.error.as_str()))
        .collect();
    assert_eq!(failed["modloader"], "Descarga dañada.");
    assert_eq!(failed["radar"], "Falló Modloader.");
    assert!(!failed.contains_key("asi-loader"));
    assert_eq!(fx.read("vorbisFile.dll").unwrap(), SILENT);
    assert!(fx.read("modloader.asi").is_none());
    fx.assert_no_litter();

    let health = fx.check(&catalog).await;
    assert!(health.blocking && fx.state.is_blocking());
    let modloader = Fx::status(&health, "modloader");
    assert!(modloader.blocking && modloader.error.is_some());
    assert!(
        !Fx::status(&health, "radar").blocking,
        "optional: never blocks"
    );
    assert!(!health.pending, "failed mods wait for a retry");
    // The automatic flow leaves them alone...
    fx.server.forget_requests();
    assert!(fx.auto(&catalog).await.ok);
    assert!(fx.server.requests().is_empty());
    // ...until "Reintentar".
    fx.server.clear_faults();
    let retried = fx.run(&catalog, Request::Auto { retry: true }).await;
    assert!(retried.ok, "{retried:?}");
    let health = fx.check(&catalog).await;
    assert!(!health.blocking && !fx.state.is_blocking());
    assert_eq!(Fx::status(&health, "radar").state, State::Installed);

    // An optional mod failing never blocks.
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    fx.server
        .fault("/pkg/radar/lq-2.0.0.zip", Some(Fault::Status(404)));
    assert!(!fx.auto(&catalog).await.ok);
    let health = fx.check(&catalog).await;
    assert!(!health.blocking);
    assert!(Fx::status(&health, "radar").error.is_some());
}

#[tokio::test]
async fn a_file_that_does_not_match_inside_a_good_zip_is_refused() {
    let fx = Fx::new().await;
    // The package checks out (its hash is the zip's), but the file in it is
    // not the one the catalog lists.
    let mut variant = fx.variant(
        "x",
        "default",
        "1",
        "game",
        &[f("x.asi", b"the real x")],
        &[],
    );
    let wrong = build_zip(&[("x.asi", b"something else")]);
    fx.server.put("/pkg/x/default-1.zip", wrong.clone());
    variant["package"]["bytes"] = json!(wrong.len());
    variant["package"]["sha256"] = json!(sha(&wrong));
    let catalog = fx.catalog(vec![json!({
        "id": "x", "name": "X", "required": true, "defaultVariant": "default", "variants": [variant]
    })]);
    let result = fx.auto(&catalog).await;
    assert_eq!(result.failed[0].error, "Descarga dañada.");
    assert!(fx.read("x.asi").is_none());
    fx.assert_no_litter();
}

#[tokio::test]
async fn only_listed_files_leave_the_package() {
    let fx = Fx::new().await;
    let before = fx.game_tree();
    let variant = fx.variant(
        "x",
        "default",
        "1",
        "modloader",
        &[f("x/x.txd", b"x")],
        &[
            ("../../evil.dll", b"evil"),
            ("C:/evil.dll", b"evil"),
            ("/abs.dll", b"evil"),
            ("..\\evil2.dll", b"evil"),
            ("readme.txt", b"hello"),
            ("x/extra.txd", b"not listed"),
            ("modloader/x/x.txd", b"a wrapper folder copy"),
        ],
    );
    let catalog = fx.catalog(vec![json!({
        "id": "x", "name": "X", "required": true, "defaultVariant": "default", "variants": [variant]
    })]);
    assert!(fx.auto(&catalog).await.ok);
    let mut expected = before;
    expected.extend([
        "modloader/".to_string(),
        "modloader/x/".into(),
        "modloader/x/x.txd".into(),
    ]);
    expected.sort();
    assert_eq!(fx.game_tree(), expected);
    assert_eq!(fx.read("modloader/x/x.txd").unwrap(), b"x");
    let outside: Vec<String> = Fx::tree(&fx.root)
        .into_iter()
        .filter(|p| p.contains("evil") || p.ends_with("readme.txt"))
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
}

#[tokio::test]
async fn keep_files_are_written_once_and_never_touched_again() {
    let fx = Fx::new().await;
    let make = |version: &str, asi: &[u8], ini: &[u8]| {
        json!({
            "id": "tool", "name": "Tool", "defaultVariant": "default", "enabledByDefault": true,
            "variants": [fx.variant("tool", "default", version, "game",
                &[f("tool/tool.asi", asi), keep("tool/tool.ini", ini)], &[])]
        })
    };
    let v1 = fx.catalog(vec![make("1", b"tool v1", b"defaults v1")]);
    assert!(fx.auto(&v1).await.ok);
    assert_eq!(fx.read("tool/tool.ini").unwrap(), b"defaults v1");
    // The user edits it; a new version ships other defaults.
    fs::write(fx.game.join("tool/tool.ini"), b"my settings").unwrap();
    let v2 = fx.catalog(vec![make("2", b"tool v2", b"defaults v2")]);
    let health = fx.check(&v2).await;
    assert_eq!(Fx::status(&health, "tool").action, Action::Update);
    assert!(fx.auto(&v2).await.ok);
    assert_eq!(fx.read("tool/tool.asi").unwrap(), b"tool v2");
    assert_eq!(fx.read("tool/tool.ini").unwrap(), b"my settings");
    assert_eq!(
        Fx::status(&fx.check(&v2).await, "tool").state,
        State::Installed
    );
    // Removing the mod leaves the user's config (and so its folder).
    let off = fx
        .run(
            &v2,
            Request::Enable {
                id: "tool".into(),
                enabled: false,
            },
        )
        .await;
    assert!(off.ok, "{off:?}");
    assert!(fx.read("tool/tool.asi").is_none());
    assert_eq!(fx.read("tool/tool.ini").unwrap(), b"my settings");
    assert!(!fx.records().contains_key("tool"));
}

#[tokio::test]
async fn a_replaced_game_file_comes_back_on_uninstall() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(vec![json!({
        "id": "loader", "name": "Loader", "enabledByDefault": true, "defaultVariant": "default",
        "variants": [fx.variant("loader", "default", "1", "game",
            &[f("vorbisFile.dll", SILENT), f("loader/plugin.asi", b"plugin")], &[])]
    })]);
    assert!(fx.auto(&catalog).await.ok);
    assert_eq!(fx.read("vorbisFile.dll").unwrap(), SILENT);
    let result = fx
        .run(
            &catalog,
            Request::Enable {
                id: "loader".into(),
                enabled: false,
            },
        )
        .await;
    assert!(result.ok, "{result:?}");
    assert_eq!(
        fx.read("vorbisFile.dll").unwrap(),
        b"the game's own vorbisFile.dll"
    );
    assert!(fx.read("loader/plugin.asi").is_none());
    assert!(!fx.game.join("loader").exists(), "emptied folders go");
    assert!(!fx.records().contains_key("loader"));
    assert!(
        Fx::tree(&fx.backups).is_empty(),
        "no backups left over: {:?}",
        Fx::tree(&fx.backups)
    );
    let health = fx.check(&catalog).await;
    let status = Fx::status(&health, "loader");
    assert!(!status.enabled);
    assert_eq!(
        (status.state, status.action),
        (State::NotInstalled, Action::None)
    );
}

#[tokio::test]
async fn our_exact_files_found_in_place_are_adopted() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    // Radar HD put there by hand, byte for byte ours.
    fs::create_dir_all(fx.game.join("modloader/radar_sarp")).unwrap();
    fs::write(fx.game.join("modloader/radar_sarp/radar00.txd"), RADAR_HD).unwrap();
    fs::write(fx.game.join("modloader.asi"), ML_ASI).unwrap();
    let health = fx.check(&catalog).await;
    let radar = Fx::status(&health, "radar");
    assert_eq!(radar.state, State::Installed);
    assert_eq!(
        radar
            .installed
            .as_ref()
            .map(|i| (i.variant.as_str(), i.adopted)),
        Some(("hd", true))
    );
    assert_eq!(radar.variant, "hd", "the variant they have is kept");
    assert_eq!(radar.action, Action::None);
    assert_eq!(Fx::status(&health, "modloader").state, State::Installed);
    assert!(fx.records()["radar"].adopted);
    // Only the ASI loader is missing.
    fx.server.forget_requests();
    assert!(fx.auto(&catalog).await.ok);
    let paths: Vec<String> = fx.server.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/pkg/asi-loader/silent-1.3.zip"]);
}

#[tokio::test]
async fn damaged_files_are_repaired() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    assert!(fx.auto(&catalog).await.ok);
    fs::write(fx.game.join("modloader/radar_sarp/radar00.txd"), b"broken").unwrap();
    fs::remove_file(fx.game.join("modloader.asi")).unwrap();
    let health = fx.check(&catalog).await;
    assert!(health.pending);
    for id in ["radar", "modloader"] {
        let status = Fx::status(&health, id);
        assert_eq!(
            (status.state, status.action),
            (State::Damaged, Action::Repair),
            "{id}"
        );
    }
    assert!(fx.auto(&catalog).await.ok);
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_LQ
    );
    assert_eq!(fx.read("modloader.asi").unwrap(), ML_ASI);
    assert!(fx
        .check(&catalog)
        .await
        .mods
        .iter()
        .all(|m| m.state == State::Installed));
}

#[tokio::test]
async fn a_new_version_replaces_the_old_files() {
    let fx = Fx::new().await;
    let make = |version: &str, files: &[F]| {
        json!({
            "id": "skin", "name": "Skin", "required": true, "defaultVariant": "default",
            "variants": [fx.variant("skin", "default", version, "modloader", files, &[])]
        })
    };
    let v1 = fx.catalog(vec![make(
        "1",
        &[f("skin/a.txd", b"a1"), f("skin/old/b.txd", b"b1")],
    )]);
    assert!(fx.auto(&v1).await.ok);
    let v2 = fx.catalog(vec![make(
        "2",
        &[f("skin/a.txd", b"a2"), f("skin/c.txd", b"c2")],
    )]);
    let health = fx.check(&v2).await;
    let skin = Fx::status(&health, "skin");
    assert_eq!((skin.state, skin.action), (State::Outdated, Action::Update));
    assert!(fx.auto(&v2).await.ok);
    assert_eq!(fx.read("modloader/skin/a.txd").unwrap(), b"a2");
    assert_eq!(fx.read("modloader/skin/c.txd").unwrap(), b"c2");
    assert!(fx.read("modloader/skin/old/b.txd").is_none());
    assert!(!fx.game.join("modloader/skin/old").exists());
    assert_eq!(fx.records()["skin"].version, "2");
    assert_eq!(fx.records()["skin"].files.len(), 2);
    fx.assert_no_litter();
}

#[tokio::test]
async fn switching_variants_swaps_the_files_both_ways() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    assert!(fx.auto(&catalog).await.ok);
    let hd = fx
        .run(
            &catalog,
            Request::Variant {
                id: "radar".into(),
                variant: "hd".into(),
            },
        )
        .await;
    assert!(hd.ok, "{hd:?}");
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_HD
    );
    assert!(fx.read("modloader/radar_sarp/lq-only.txd").is_none());
    assert_eq!(fx.records()["radar"].variant, "hd");
    assert_eq!(
        fx.store.get().mods_prefs["radar"].variant.as_deref(),
        Some("hd")
    );
    let lq = fx
        .run(
            &catalog,
            Request::Variant {
                id: "radar".into(),
                variant: "lq".into(),
            },
        )
        .await;
    assert!(lq.ok);
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_LQ
    );
    assert_eq!(fx.read("modloader/radar_sarp/lq-only.txd").unwrap(), b"lq");
    let bad = fx
        .run(
            &catalog,
            Request::Variant {
                id: "radar".into(),
                variant: "4k".into(),
            },
        )
        .await;
    assert_eq!(bad.error.as_deref(), Some("Esa variante no existe."));
    fx.assert_no_litter();
}

#[tokio::test]
async fn removing_a_mod_leaves_everything_else_alone() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    assert!(fx.auto(&catalog).await.ok);
    // The user's own mods next to ours, and one of our files they edited.
    fs::create_dir_all(fx.game.join("modloader/my_cars")).unwrap();
    fs::write(fx.game.join("modloader/my_cars/infernus.dff"), b"mine").unwrap();
    fs::write(fx.game.join("modloader/radar_sarp/mine.txd"), b"also mine").unwrap();
    fs::write(
        fx.game.join("modloader/radar_sarp/lq-only.txd"),
        b"edited by the user",
    )
    .unwrap();
    let off = fx
        .run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false,
            },
        )
        .await;
    assert!(off.ok, "{off:?}");
    assert!(fx.read("modloader/radar_sarp/radar00.txd").is_none());
    assert_eq!(
        fx.read("modloader/radar_sarp/lq-only.txd").unwrap(),
        b"edited by the user"
    );
    assert_eq!(
        fx.read("modloader/radar_sarp/mine.txd").unwrap(),
        b"also mine"
    );
    assert_eq!(fx.read("modloader/my_cars/infernus.dff").unwrap(), b"mine");
    assert_eq!(
        fx.read("modloader.asi").unwrap(),
        ML_ASI,
        "the other mods stay"
    );
    assert!(!fx.records().contains_key("radar"));
    // Turned off: never installed again by the automatic flow.
    let health = fx.check(&catalog).await;
    assert!(!health.pending);
    assert!(!Fx::status(&health, "radar").enabled);
    // And back on.
    let on = fx
        .run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: true,
            },
        )
        .await;
    assert!(on.ok, "{on:?}");
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_LQ
    );
}

#[tokio::test]
async fn dependencies_install_first_and_lock_what_needs_them() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(vec![
        json!({
            "id": "radar", "name": "Radar", "requires": ["lib"], "defaultVariant": "default",
            "variants": [fx.variant("radar", "default", "1", "game", &[f("radar.asi", b"r")], &[])]
        }),
        json!({
            "id": "lib", "name": "Lib", "defaultVariant": "default",
            "variants": [fx.variant("lib", "default", "1", "game", &[f("lib.asi", b"l")], &[])]
        }),
        json!({
            "id": "base", "name": "Base", "required": true, "defaultVariant": "default",
            "variants": [fx.variant("base", "default", "1", "game", &[f("base.asi", b"b")], &[])]
        }),
    ]);
    let refused = fx
        .run(
            &catalog,
            Request::Enable {
                id: "base".into(),
                enabled: false,
            },
        )
        .await;
    assert_eq!(refused.error.as_deref(), Some("Base es esencial."));
    fx.server.forget_requests();
    let on = fx
        .run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: true,
            },
        )
        .await;
    assert!(on.ok, "{on:?}");
    let paths: Vec<String> = fx.server.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        ["/pkg/lib/default-1.zip", "/pkg/radar/default-1.zip"]
    );
    let prefs = fx.store.get().mods_prefs;
    assert!(prefs["lib"].enabled && prefs["radar"].enabled);
    assert!(
        fx.read("base.asi").is_none(),
        "only what was asked for (and needed)"
    );

    let refused = fx
        .run(
            &catalog,
            Request::Enable {
                id: "lib".into(),
                enabled: false,
            },
        )
        .await;
    assert_eq!(refused.error.as_deref(), Some("Lo necesita Radar."));
    assert_eq!(fx.read("lib.asi").unwrap(), b"l");
    let health = fx.check(&catalog).await;
    assert_eq!(Fx::status(&health, "lib").required_by, ["Radar"]);
    assert!(
        fx.run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert!(
        fx.run(
            &catalog,
            Request::Enable {
                id: "lib".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert!(fx.read("lib.asi").is_none() && fx.read("radar.asi").is_none());

    // A dependency that isn't in the catalog: blocked, can't be enabled.
    let lonely = fx.catalog(vec![json!({
        "id": "lonely", "name": "Lonely", "requires": ["ghost"], "defaultVariant": "default",
        "variants": [fx.variant("lonely", "default", "1", "game", &[f("lonely.asi", b"x")], &[])]
    })]);
    let health = fx.check(&lonely).await;
    assert_eq!(Fx::status(&health, "lonely").state, State::Blocked);
    let refused = fx
        .run(
            &lonely,
            Request::Enable {
                id: "lonely".into(),
                enabled: true,
            },
        )
        .await;
    assert_eq!(refused.error.as_deref(), Some("Falta ghost."));
}

#[tokio::test]
async fn asi_loaders_installed_by_hand_are_left_alone() {
    // Ultimate ASI Loader (dinput8.dll).
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    fs::write(fx.game.join("dinput8.dll"), b"Ultimate ASI Loader").unwrap();
    let health = fx.check(&catalog).await;
    let asi = Fx::status(&health, "asi-loader");
    assert_eq!((asi.state, asi.action), (State::Foreign, Action::None));
    assert!(!asi.blocking);
    assert!(fx.auto(&catalog).await.ok);
    assert_eq!(fx.read("dinput8.dll").unwrap(), b"Ultimate ASI Loader");
    assert_eq!(
        fx.read("vorbisFile.dll").unwrap(),
        b"the game's own vorbisFile.dll"
    );

    // Silent's (vorbisFile.dll + vorbisHooked.dll): note every valid GTA
    // folder has both files, so this set matches any install.
    let fx = Fx::new().await;
    let mut mods = real_mods(&fx);
    mods[1]["equivalents"] = json!([["vorbisFile.dll", "vorbisHooked.dll"], ["dinput8.dll"]]);
    let catalog = fx.catalog(mods);
    fs::write(fx.game.join("vorbisFile.dll"), b"Silent's ASI Loader 1.2").unwrap();
    let health = fx.check(&catalog).await;
    assert_eq!(Fx::status(&health, "asi-loader").state, State::Foreign);
    assert!(fx.auto(&catalog).await.ok);
    assert_eq!(
        fx.read("vorbisFile.dll").unwrap(),
        b"Silent's ASI Loader 1.2"
    );
    // "Instalar versión recomendada": ours goes in, theirs to the backups.
    let ours = fx
        .run(
            &catalog,
            Request::Recommended {
                id: "asi-loader".into(),
            },
        )
        .await;
    assert!(ours.ok, "{ours:?}");
    assert_eq!(fx.read("vorbisFile.dll").unwrap(), SILENT);
    assert!(
        fx.records()["asi-loader"]
            .owns("vorbisFile.dll")
            .unwrap()
            .backup
    );
    assert_eq!(
        Fx::status(&fx.check(&catalog).await, "asi-loader").state,
        State::Installed
    );
    let again = fx
        .run(
            &catalog,
            Request::Recommended {
                id: "asi-loader".into(),
            },
        )
        .await;
    assert!(again.error.unwrap().contains("No aplica"));
}

#[tokio::test]
async fn a_clashing_radar_is_renamed_out_of_the_way_and_back() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    fs::create_dir_all(fx.game.join("modloader/radar_hq")).unwrap();
    fs::write(
        fx.game.join("modloader/radar_hq/radar00.txd"),
        b"a hand-made HQ radar",
    )
    .unwrap();
    assert!(fx.auto(&catalog).await.ok);
    let health = fx.check(&catalog).await;
    let radar = Fx::status(&health, "radar");
    assert_eq!(radar.conflicts, ["modloader/radar_hq"]);
    assert!(radar.disabled_conflicts.is_empty());

    let disabled = fx
        .run(&catalog, Request::Conflicts { id: "radar".into() })
        .await;
    assert!(disabled.ok, "{disabled:?}");
    assert!(!fx.game.join("modloader/radar_hq").exists());
    assert_eq!(
        fx.read("modloader/.radar_hq/radar00.txd").unwrap(),
        b"a hand-made HQ radar"
    );
    let health = fx.check(&catalog).await;
    let radar = Fx::status(&health, "radar");
    assert!(radar.conflicts.is_empty());
    assert_eq!(radar.disabled_conflicts, ["modloader/radar_hq"]);
    let nothing = fx
        .run(&catalog, Request::Conflicts { id: "radar".into() })
        .await;
    assert_eq!(nothing.error.as_deref(), Some("Nada que desactivar."));

    // Turning our radar off brings theirs back.
    assert!(
        fx.run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert_eq!(
        fx.read("modloader/radar_hq/radar00.txd").unwrap(),
        b"a hand-made HQ radar"
    );
    assert!(!fx.game.join("modloader/.radar_hq").exists());
    assert!(!fx.records().contains_key("radar"));
}

#[tokio::test]
async fn nothing_is_touched_while_the_game_runs() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    let before = fx.game_tree();
    fx.running.store(true, Ordering::SeqCst);
    for request in [
        Request::Auto { retry: false },
        Request::Enable {
            id: "radar".into(),
            enabled: false,
        },
        Request::Variant {
            id: "radar".into(),
            variant: "hd".into(),
        },
    ] {
        let result = fx.run(&catalog, request).await;
        assert_eq!(result.error.as_deref(), Some("Cierra el juego primero."));
    }
    assert_eq!(fx.game_tree(), before);
    assert!(
        fx.store.get().mods_prefs.is_empty(),
        "not even the choices change"
    );
    assert!(fx.server.requests().is_empty());
    // Checking is read-only and still works.
    assert!(fx.check(&catalog).await.pending);
}

#[tokio::test]
async fn a_protected_folder_goes_through_one_script() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    fx.writable.store(false, Ordering::SeqCst);
    let result = fx.auto(&catalog).await;
    assert!(result.ok, "{result:?}");
    assert_eq!(fx.read("modloader.asi").unwrap(), ML_ASI);
    assert_eq!(fx.read("vorbisFile.dll").unwrap(), SILENT);
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_LQ
    );
    assert!(fx.events.lock().unwrap().iter().any(|(_, p)| p
        .message
        .as_deref()
        .is_some_and(|m| m.starts_with("Acepta el aviso de Windows"))));
    // Switch and removal through the script too, conflicts included.
    fs::create_dir_all(fx.game.join("modloader/radar_lq")).unwrap();
    assert!(
        fx.run(
            &catalog,
            Request::Variant {
                id: "radar".into(),
                variant: "hd".into()
            }
        )
        .await
        .ok
    );
    assert_eq!(
        fx.read("modloader/radar_sarp/radar00.txd").unwrap(),
        RADAR_HD
    );
    assert!(
        fx.run(&catalog, Request::Conflicts { id: "radar".into() })
            .await
            .ok
    );
    assert!(fx.game.join("modloader/.radar_lq").is_dir());
    assert!(
        fx.run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert!(!fx.game.join("modloader/radar_sarp").exists());
    assert!(fx.game.join("modloader/radar_lq").is_dir());
    fx.assert_no_litter();
}

#[tokio::test]
async fn without_a_game_folder_only_the_choices_change() {
    let fx = Fx::new().await;
    let catalog = fx.catalog(real_mods(&fx));
    let mut engine = fx.engine();
    engine.dir = None;
    let loaded = Loaded {
        catalog: Some(catalog.clone()),
        origin: Origin::Live,
        problem: None,
    };
    let health = engine.check(&loaded).await;
    assert!(!health.ready && !health.pending);
    let off = engine
        .run(
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false,
            },
        )
        .await;
    assert!(off.ok);
    assert!(!fx.store.get().mods_prefs["radar"].enabled);
    let refused = engine
        .run(&catalog, Request::Conflicts { id: "radar".into() })
        .await;
    assert!(refused.error.is_some());
    assert!(fx.server.requests().is_empty());
}

#[tokio::test]
async fn a_cancelled_run_leaves_nothing_half_done() {
    let fx = Fx::new().await;
    // Incompressible, so the package is as big as the file.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let big: Vec<u8> = (0..256 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let catalog = fx.catalog(vec![json!({
        "id": "big", "name": "Big", "required": true, "defaultVariant": "default",
        "variants": [fx.variant("big", "default", "1", "game", &[f("big.asi", &big)], &[])]
    })]);
    fx.server
        .fault("/pkg/big/default-1.zip", Some(Fault::Trickle(4096, 30)));
    let engine = fx.engine();
    let (result, ()) = tokio::join!(
        engine.run(&catalog, Request::Auto { retry: false }),
        async {
            // Once the package is on its way (a busy machine may take a
            // while to get there).
            let started = std::time::Instant::now();
            while fx.server.sent("/pkg/big/default-1.zip") == 0 {
                assert!(started.elapsed() < std::time::Duration::from_secs(20));
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(fx.state.cancel());
        }
    );
    assert_eq!(result.cancelled, Some(true), "{result:?}");
    assert!(!result.ok);
    assert!(fx.read("big.asi").is_none());
    assert!(!fx.state.is_busy());
    assert!(!fx.state.cancel(), "nothing running any more");
}

#[tokio::test]
async fn backups_are_per_game_folder() {
    let fx = Fx::new().await;
    let engine = fx.engine();
    let a = engine.backups_dir(Path::new(r"C:\Games\GTA"));
    assert_eq!(a, engine.backups_dir(Path::new("c:/games/gta/")));
    assert_ne!(a, engine.backups_dir(Path::new(r"D:\GTA")));
    assert_eq!(a.parent().unwrap(), fx.backups);
}

#[path = "preserve_tests.rs"]
mod preserve;
