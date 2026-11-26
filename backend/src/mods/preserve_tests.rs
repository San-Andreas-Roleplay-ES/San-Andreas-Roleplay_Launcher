//! Nothing in `modloader/` that isn't one of a mod's own listed files is ever
//! deleted, moved or overwritten: every operation is checked against a
//! byte-for-byte snapshot of a realistic, populated folder (user mods in
//! nested folders, Modloader's own config, profiles, logs, a dot-folder, an
//! empty folder, read-only files, a folder whose name differs from ours only
//! by case).

use std::collections::BTreeMap;

use super::*;

/// Every file and folder under `root`: `rel` → `dir`, or the content hash
/// and the read-only flag. Links are not followed. Names as spelled on disk.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                out.insert(format!("{rel}/"), "dir".into());
                walk(base, &path, out);
            } else {
                let readonly = entry.metadata().unwrap().permissions().readonly();
                out.insert(
                    rel,
                    format!("{} ro={readonly}", sha(&fs::read(&path).unwrap())),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// `snapshot` without the paths in `own` (lower-case; a trailing `/` is a
/// folder and everything in it).
fn without(snap: &BTreeMap<String, String>, own: &[String]) -> BTreeMap<String, String> {
    snap.iter()
        .filter(|(rel, _)| {
            let lower = rel.to_ascii_lowercase();
            !own.iter().any(|o| {
                lower == *o
                    || (o.ends_with('/') && lower.starts_with(o.as_str()))
                    || lower.trim_end_matches('/') == o.trim_end_matches('/')
            })
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(readonly);
    fs::set_permissions(path, permissions).unwrap();
}

/// A modloader folder like a real player's.
fn populate(game: &Path) {
    let ml = game.join("modloader");
    let files: &[(&str, &[u8])] = &[
        ("modloader.ini", b"[Folder.Config]\nProfile=Default\n"),
        ("modloader.log", b"modloader log of the last session"),
        (".data/config.ini", b"[Config]\nEnableMenu=true\n"),
        (".data/plugins.ini", b"[Priority]\nstd.fx=50\n"),
        (
            ".profiles/Default/Profile.ini",
            b"[Config]\nIgnoreAllMods=false\n",
        ),
        ("Cars/Infernus/infernus.dff", b"user infernus model"),
        ("Cars/Infernus/infernus.txd", b"user infernus textures"),
        ("Cars/readme.txt", b"my cars"),
        ("Weapons/M4/m4.dff", b"user m4"),
        (".disabled/OldMod/old.txd", b"an old mod the user disabled"),
        ("ReadOnly/locked.txt", b"a read-only user file"),
        (
            "sarp_minimap/user-note.txt",
            b"the user's note in a folder like ours",
        ),
        ("Radar_HQ/radar00.txd", b"a hand-made HQ radar"),
    ];
    for (rel, body) in files {
        let path = ml.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }
    fs::create_dir_all(ml.join("EmptyUserDir")).unwrap();
    set_readonly(&ml.join("ReadOnly/locked.txt"), true);
}

/// Read-only files must be writable again or the temp folder stays behind.
fn release(game: &Path) {
    for (rel, value) in snapshot(game) {
        if value.ends_with("ro=true") {
            set_readonly(&game.join(rel), false);
        }
    }
}

const STD_ASI: &[u8] = b"modloader std.asi plugin";
const CONFIG0: &[u8] = b"[Config] default template";
const RADAR00: &[u8] = b"SARP radar tile 00 (LQ)";
const RADAR01: &[u8] = b"SARP radar tile 01 (LQ)";
const EXTRA: &[u8] = b"SARP radar extra tile";
const RADAR00_HD: &[u8] = b"SARP radar tile 00 (HD!)";
const CAR: &[u8] = b"SARP car model";

/// Modloader (required, its package writes inside `modloader/.data`), a radar
/// and a cars pack into `modloader/` (optional).
fn mods(fx: &Fx, radar_lq: &[F]) -> Vec<Value> {
    vec![
        json!({
            "id": "modloader", "name": "Modloader", "required": true,
            "equivalents": [["modloader.asi", "modloader/"]], "defaultVariant": "default",
            "variants": [fx.variant("modloader", "default", "0.3.10", "game", &[
                f("modloader.asi", ML_ASI),
                f("modloader/.data/plugins/gta3/std.asi.dll", STD_ASI),
                f("modloader/.data/config.ini.0", CONFIG0),
            ], &[])]
        }),
        json!({
            "id": "radar", "name": "Radar", "enabledByDefault": true, "requires": ["modloader"],
            "replaces": ["modloader/radar_hq"], "defaultVariant": "lq",
            "variants": [
                fx.variant("radar", "lq", &format!("{}", radar_lq.len()), "modloader", radar_lq, &[]),
                fx.variant("radar", "hd", "1", "modloader", &[f("SARP_Minimap/radar00.txd", RADAR00_HD)], &[]),
            ]
        }),
        json!({
            "id": "cars", "name": "Cars", "requires": ["modloader"], "defaultVariant": "default",
            "variants": [fx.variant("cars", "default", "1", "modloader", &[f("SARP_Cars/car.dff", CAR)], &[])]
        }),
    ]
}

fn radar_v1() -> Vec<F> {
    vec![
        f("SARP_Minimap/radar00.txd", RADAR00),
        f("SARP_Minimap/radar01.txd", RADAR01),
        f("SARP_Minimap/sub/extra.txd", EXTRA),
    ]
}

/// Every file any variant of these mods lists, and the folders the launcher
/// created for them (from the records).
fn own(fx: &Fx, catalog: &Catalog) -> Vec<String> {
    let mut own: Vec<String> = catalog
        .mods
        .iter()
        .flat_map(|m| m.variants.iter().flat_map(|v| v.files.iter()))
        .map(|f| f.path.to_ascii_lowercase())
        .collect();
    for record in fx.records().values() {
        own.extend(record.files.iter().map(|f| f.path.to_ascii_lowercase()));
        own.extend(
            record
                .dirs
                .iter()
                .map(|d| format!("{}/", d.to_ascii_lowercase())),
        );
    }
    own
}

/// Runs `step` and checks nothing but the mods' own files changed.
async fn unchanged_but_ours<Fut: std::future::Future<Output = ModsSyncResult>>(
    fx: &Fx,
    catalog: &Catalog,
    what: &str,
    step: impl FnOnce() -> Fut,
) {
    let before = snapshot(&fx.game);
    let created_before = own(fx, catalog);
    let result = step().await;
    assert!(result.ok, "{what}: {result:?}");
    let mut owned = own(fx, catalog);
    owned.extend(created_before);
    let after = snapshot(&fx.game);
    assert_eq!(without(&after, &owned), without(&before, &owned), "{what}");
    fx.assert_no_litter();
}

#[tokio::test]
async fn every_operation_leaves_the_users_modloader_folder_alone() {
    let fx = Fx::new().await;
    populate(&fx.game);
    let catalog = fx.catalog(mods(&fx, &radar_v1()));

    // Install (Modloader + the default radar): `sarp_minimap` is the user's
    // folder (our SARP_Minimap on Windows), so ours go in next to their note.
    unchanged_but_ours(&fx, &catalog, "install", || fx.auto(&catalog)).await;
    assert_eq!(
        fx.read("modloader/SARP_Minimap/radar00.txd").unwrap(),
        RADAR00
    );
    assert_eq!(fx.records()["radar"].dirs, ["modloader/SARP_Minimap/sub"]);
    assert!(fx.records()["modloader"]
        .dirs
        .iter()
        .all(|d| d.contains(".data/plugins")));

    // Repair.
    fs::write(
        fx.game.join("modloader/SARP_Minimap/radar01.txd"),
        b"broken",
    )
    .unwrap();
    unchanged_but_ours(&fx, &catalog, "repair", || fx.auto(&catalog)).await;

    // Update: a tile and the nested folder dropped, a tile added.
    let v2 = fx.catalog(mods(
        &fx,
        &[
            f("SARP_Minimap/radar00.txd", RADAR00),
            f("SARP_Minimap/radar02.txd", b"new tile"),
        ],
    ));
    unchanged_but_ours(&fx, &v2, "update", || fx.auto(&v2)).await;
    assert!(
        !fx.game.join("modloader/SARP_Minimap/sub").exists(),
        "our emptied folder goes"
    );
    assert!(fx
        .game
        .join("modloader/sarp_minimap/user-note.txt")
        .exists());

    // Switch and back.
    unchanged_but_ours(&fx, &v2, "switch to HD", || {
        run_on(
            &fx,
            &v2,
            Request::Variant {
                id: "radar".into(),
                variant: "hd".into(),
            },
        )
    })
    .await;
    unchanged_but_ours(&fx, &v2, "switch back", || {
        run_on(
            &fx,
            &v2,
            Request::Variant {
                id: "radar".into(),
                variant: "lq".into(),
            },
        )
    })
    .await;

    // A pack in a folder of its own: in and out, and that folder goes.
    unchanged_but_ours(&fx, &v2, "cars on", || {
        run_on(
            &fx,
            &v2,
            Request::Enable {
                id: "cars".into(),
                enabled: true,
            },
        )
    })
    .await;
    unchanged_but_ours(&fx, &v2, "cars off", || {
        run_on(
            &fx,
            &v2,
            Request::Enable {
                id: "cars".into(),
                enabled: false,
            },
        )
    })
    .await;
    assert!(!fx.game.join("modloader/SARP_Cars").exists());

    // The hand-made radar renamed out of the way, with the user's spelling,
    // and back when our radar goes.
    let before = snapshot(&fx.game);
    let result = run_on(&fx, &v2, Request::Conflicts { id: "radar".into() }).await;
    assert!(result.ok, "{result:?}");
    assert!(fx.game.join("modloader/.Radar_HQ/radar00.txd").is_file());
    // Exactly a rename: same files, same bytes, only the folder's name.
    let expected: BTreeMap<String, String> = before
        .into_iter()
        .map(|(k, v)| (k.replace("modloader/Radar_HQ", "modloader/.Radar_HQ"), v))
        .collect();
    assert_eq!(snapshot(&fx.game), expected);
    // Our radar off: its files go and theirs is renamed back, nothing else.
    let before = snapshot(&fx.game);
    let owned = own(&fx, &v2);
    let result = run_on(
        &fx,
        &v2,
        Request::Enable {
            id: "radar".into(),
            enabled: false,
        },
    )
    .await;
    assert!(result.ok, "{result:?}");
    let expected: BTreeMap<String, String> = without(&before, &owned)
        .into_iter()
        .map(|(k, v)| (k.replace("modloader/.Radar_HQ", "modloader/Radar_HQ"), v))
        .collect();
    assert_eq!(without(&snapshot(&fx.game), &owned), expected, "radar off");
    let names: Vec<String> = fs::read_dir(fx.game.join("modloader"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"Radar_HQ".to_string()),
        "back with the user's spelling: {names:?}"
    );
    assert!(fx
        .game
        .join("modloader/sarp_minimap/user-note.txt")
        .is_file());
    assert!(fx.game.join("modloader/EmptyUserDir").is_dir());
    release(&fx.game);
}

async fn run_on(fx: &Fx, catalog: &Arc<Catalog>, request: Request) -> ModsSyncResult {
    fx.run(catalog, request).await
}

#[tokio::test]
async fn the_same_through_the_elevated_script() {
    let fx = Fx::new().await;
    populate(&fx.game);
    fx.writable.store(false, Ordering::SeqCst);
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    unchanged_but_ours(&fx, &catalog, "install", || fx.auto(&catalog)).await;
    let v2 = fx.catalog(mods(&fx, &[f("SARP_Minimap/radar00.txd", RADAR00)]));
    unchanged_but_ours(&fx, &v2, "update", || fx.auto(&v2)).await;
    unchanged_but_ours(&fx, &v2, "radar off", || {
        run_on(
            &fx,
            &v2,
            Request::Enable {
                id: "radar".into(),
                enabled: false,
            },
        )
    })
    .await;
    assert!(fx
        .game
        .join("modloader/sarp_minimap/user-note.txt")
        .is_file());
    release(&fx.game);
}

#[tokio::test]
async fn our_own_modloader_over_the_users_keeps_their_config_and_mods() {
    let fx = Fx::new().await;
    populate(&fx.game);
    // Their Modloader (another build), and a std.asi plugin of their own.
    fs::write(fx.game.join("modloader.asi"), b"the user's modloader build").unwrap();
    let plugin = fx.game.join("modloader/.data/plugins/gta3/std.asi.dll");
    fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    fs::write(&plugin, b"the user's std.asi").unwrap();
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    let health = fx.check(&catalog).await;
    assert_eq!(Fx::status(&health, "modloader").state, State::Foreign);
    // Adopting or leaving it alone: not a byte changes.
    let before = snapshot(&fx.game);
    fx.check(&catalog).await;
    assert_eq!(snapshot(&fx.game), before, "a check never writes");

    unchanged_but_ours(&fx, &catalog, "recommended", || {
        run_on(
            &fx,
            &catalog,
            Request::Recommended {
                id: "modloader".into(),
            },
        )
    })
    .await;
    assert_eq!(fx.read("modloader.asi").unwrap(), ML_ASI);
    assert_eq!(
        fx.read("modloader/.data/config.ini").unwrap(),
        b"[Config]\nEnableMenu=true\n"
    );
    assert_eq!(
        fx.read("modloader/.data/plugins.ini").unwrap(),
        b"[Priority]\nstd.fx=50\n"
    );
    // What it replaced is kept for an uninstall.
    let record = &fx.records()["modloader"];
    assert!(record.owns("modloader.asi").unwrap().backup);
    assert!(
        record
            .owns("modloader/.data/plugins/gta3/std.asi.dll")
            .unwrap()
            .backup
    );
    assert!(record.dirs.is_empty(), "every folder was already there");
    release(&fx.game);
}

#[tokio::test]
async fn adopting_writes_nothing() {
    let fx = Fx::new().await;
    populate(&fx.game);
    for (rel, body) in [
        ("modloader.asi", ML_ASI),
        ("modloader/.data/plugins/gta3/std.asi.dll", STD_ASI),
        ("modloader/.data/config.ini.0", CONFIG0),
    ] {
        let path = fx.game.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    let before = snapshot(&fx.game);
    let health = fx.check(&catalog).await;
    assert_eq!(Fx::status(&health, "modloader").state, State::Installed);
    assert!(fx.records()["modloader"].adopted);
    assert_eq!(snapshot(&fx.game), before);
    release(&fx.game);
}

#[tokio::test]
async fn modloader_itself_never_goes() {
    // No modloader folder at all: a pack creates it, and taking the pack out
    // removes only the pack's own folder.
    let fx = Fx::new().await;
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    fs::write(fx.game.join("modloader.asi"), b"the user's modloader build").unwrap();
    assert!(
        run_on(
            &fx,
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    let on = run_on(
        &fx,
        &catalog,
        Request::Enable {
            id: "cars".into(),
            enabled: true,
        },
    )
    .await;
    assert!(on.ok, "{on:?}");
    assert_eq!(
        fx.records()["cars"].dirs,
        ["modloader", "modloader/SARP_Cars"]
    );
    assert!(
        run_on(
            &fx,
            &catalog,
            Request::Enable {
                id: "cars".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert!(!fx.game.join("modloader/SARP_Cars").exists());
    assert!(
        fx.game.join("modloader").is_dir(),
        "modloader/ stays even if we made it"
    );
}

#[tokio::test]
async fn a_read_only_file_of_theirs_at_one_of_our_paths_comes_back_as_it_was() {
    let fx = Fx::new().await;
    populate(&fx.game);
    let theirs = fx.game.join("modloader/sarp_minimap/radar01.txd");
    fs::write(&theirs, b"their own tile 01").unwrap();
    set_readonly(&theirs, true);
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    assert!(fx.auto(&catalog).await.ok);
    assert_eq!(
        fx.read("modloader/SARP_Minimap/radar01.txd").unwrap(),
        RADAR01
    );
    assert!(
        run_on(
            &fx,
            &catalog,
            Request::Enable {
                id: "radar".into(),
                enabled: false
            }
        )
        .await
        .ok
    );
    assert_eq!(fs::read(&theirs).unwrap(), b"their own tile 01");
    assert!(
        fs::metadata(&theirs).unwrap().permissions().readonly(),
        "read-only again"
    );
    assert!(
        Fx::tree(&fx.backups).iter().all(|p| p.ends_with('/')),
        "backup used up"
    );
    release(&fx.game);
}

#[tokio::test]
async fn a_file_the_user_replaced_after_install_is_never_overwritten_on_uninstall() {
    let fx = Fx::new().await;
    populate(&fx.game);
    let theirs = fx.game.join("modloader/sarp_minimap/radar01.txd");
    fs::write(&theirs, b"their tile, before").unwrap();
    let catalog = fx.catalog(mods(&fx, &radar_v1()));
    assert!(fx.auto(&catalog).await.ok);
    // They put another file of their own over ours later.
    fs::write(&theirs, b"their tile, after").unwrap();
    fs::write(
        fx.game.join("modloader/SARP_Minimap/radar00.txd"),
        b"their edit of ours",
    )
    .unwrap();
    assert!(
        run_on(
            &fx,
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
        fs::read(&theirs).unwrap(),
        b"their tile, after",
        "not the old backup"
    );
    assert_eq!(
        fx.read("modloader/SARP_Minimap/radar00.txd").unwrap(),
        b"their edit of ours"
    );
    // The original they had before stays in the backups, never lost.
    let kept: Vec<String> = Fx::tree(&fx.backups)
        .into_iter()
        .filter(|p| p.ends_with("radar01.txd"))
        .collect();
    assert_eq!(kept.len(), 1, "{:?}", Fx::tree(&fx.backups));
    release(&fx.game);
}
