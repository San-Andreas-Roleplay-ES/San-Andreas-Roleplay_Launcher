//! The mods catalog (schema 1): every mod the launcher can install, its
//! variants, packages and files. It arrives signed (see `signature`) and is
//! validated here only after the signature checks out.
//!
//! Validation is per mod: a mod that breaks a rule is skipped (and logged),
//! the rest of the catalog still counts. Only a catalog that is not one at
//! all (or comes from a newer schema) is refused as a whole. Unknown fields
//! are ignored everywhere, so newer catalogs keep working with this launcher.

use std::collections::{HashMap, HashSet};

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

use crate::game::paths::{REQUIRED_DIRS, REQUIRED_FILES};
use crate::install::cache::check_url;
use crate::install::extract::strict_relative_path;

pub const SCHEMA_VERSION: u64 = 1;

/// Where a variant's `files[].path` are relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// The GTA folder.
    Game,
    /// `<GTA>/modloader/`.
    Modloader,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Package {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    /// Its name inside the package: the catalog's `path`.
    pub entry: String,
    /// Relative to the game folder, forward slashes (`modloader/<entry>` for
    /// that target).
    pub path: String,
    pub bytes: u64,
    /// Lower-case hex.
    pub sha256: String,
    /// User config: written only if absent; never overwritten, verified or
    /// removed.
    pub keep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub id: String,
    pub label: String,
    pub version: String,
    pub target: Target,
    pub package: Package,
    pub files: Vec<ModFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mod {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub required: bool,
    pub enabled_by_default: bool,
    pub requires: Vec<String>,
    /// Sets of game-relative paths (a trailing `/` is a folder): all of one
    /// set present = installed by other means.
    pub equivalents: Vec<Vec<String>>,
    /// Game-relative paths of older or manual installs that clash with this
    /// mod.
    pub replaces: Vec<String>,
    pub min_launcher: Option<Version>,
    pub default_variant: String,
    pub variants: Vec<Variant>,
}

impl Mod {
    pub fn variant(&self, id: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.id == id)
    }

    /// Validation guarantees it exists.
    pub fn default_variant(&self) -> &Variant {
        self.variant(&self.default_variant)
            .unwrap_or(&self.variants[0])
    }

    /// Every path any variant installs (lower-case, game-relative).
    fn all_paths(&self) -> impl Iterator<Item = String> + '_ {
        self.variants
            .iter()
            .flat_map(|v| v.files.iter().map(|f| f.path.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub serial: u64,
    pub generated_at: String,
    pub mods: Vec<Mod>,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&Mod> {
        self.mods.iter().find(|m| m.id == id)
    }
}

/// Why a (verified) catalog was refused as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Written for a newer launcher.
    TooNew(u64),
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCatalog {
    serial: u64,
    #[serde(default)]
    generated_at: Option<String>,
    mods: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMod {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    enabled_by_default: bool,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    equivalents: Vec<Vec<String>>,
    #[serde(default)]
    replaces: Vec<String>,
    #[serde(default)]
    min_launcher: Option<String>,
    default_variant: String,
    variants: Vec<RawVariant>,
}

#[derive(Deserialize)]
struct RawVariant {
    id: String,
    #[serde(default)]
    label: String,
    version: String,
    target: Target,
    package: Package,
    files: Vec<RawFile>,
}

#[derive(Deserialize)]
struct RawFile {
    path: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    keep: bool,
}

/// `^[a-z0-9][a-z0-9-]{0,39}$`
pub fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=40).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `^[0-9A-Za-z][0-9A-Za-z.+-]{0,39}$`, as the publisher writes them
/// (`0.3.10`, `2026.10.25`). Not semver: two versions are only ever equal or
/// different.
fn valid_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    (1..=40).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// Never renamed out of the way: the game and SA-MP themselves.
fn protected(path: &str) -> bool {
    let lower = path.trim_end_matches('/').to_ascii_lowercase();
    let first = lower.split('/').next().unwrap_or_default();
    REQUIRED_FILES
        .iter()
        .chain(
            [
                "samp.exe",
                "samp.dll",
                "samp.saa",
                "samp",
                "modloader",
                "modloader.asi",
            ]
            .iter(),
        )
        .any(|p| p.eq_ignore_ascii_case(&lower))
        || REQUIRED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(first))
}

/// `a` is `b` or one of its folders (lower-case, no trailing slash).
fn covers(a: &str, b: &str) -> bool {
    b == a || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

fn check_text(value: &str, field: &str, max: usize, required: bool) -> Result<String, String> {
    let value = value.trim();
    if (required && value.is_empty()) || value.chars().count() > max {
        return Err(format!("bad {field}"));
    }
    Ok(value.to_string())
}

fn parse_variant(raw: RawVariant, require_https: bool) -> Result<Variant, String> {
    if !valid_id(&raw.id) {
        return Err(format!("bad variant id {:?}", raw.id));
    }
    let label = check_text(&raw.label, "label", 40, false)?;
    if !valid_version(&raw.version) {
        return Err(format!(
            "bad version {:?} in variant {}",
            raw.version, raw.id
        ));
    }
    let version = raw.version;
    let url = check_url(&raw.package.url, require_https, true)
        .filter(|_| raw.package.bytes > 0 && valid_sha256(&raw.package.sha256))
        .ok_or_else(|| format!("bad package in variant {}", raw.id))?;
    if raw.files.is_empty() {
        return Err(format!("variant {} has no files", raw.id));
    }
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(raw.files.len());
    for file in raw.files {
        if strict_relative_path(&file.path, false).is_none() || !valid_sha256(&file.sha256) {
            return Err(format!("bad file {:?} in variant {}", file.path, raw.id));
        }
        let path = match raw.target {
            Target::Game => file.path.clone(),
            Target::Modloader => format!("modloader/{}", file.path),
        };
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate file {:?} in variant {}",
                file.path, raw.id
            ));
        }
        files.push(ModFile {
            entry: file.path,
            path,
            bytes: file.bytes,
            sha256: file.sha256.to_ascii_lowercase(),
            keep: file.keep,
        });
    }
    // A path can't be a file and a folder of another file at once.
    for a in &seen {
        if seen.iter().any(|b| b != a && covers(a, b)) {
            return Err(format!(
                "{a:?} is both a file and a folder in variant {}",
                raw.id
            ));
        }
    }
    Ok(Variant {
        id: raw.id,
        label,
        version,
        target: raw.target,
        package: Package {
            url,
            bytes: raw.package.bytes,
            sha256: raw.package.sha256.to_ascii_lowercase(),
        },
        files,
    })
}

fn parse_mod(value: Value, require_https: bool) -> Result<Mod, String> {
    let raw: RawMod = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if !valid_id(&raw.id) {
        return Err("bad id".into());
    }
    let name = check_text(&raw.name, "name", 80, true)?;
    let description = check_text(&raw.description, "description", 400, false)?;
    let category = check_text(&raw.category, "category", 40, false)?.to_ascii_lowercase();
    if raw.requires.iter().any(|r| !valid_id(r) || *r == raw.id) {
        return Err("bad requires".into());
    }
    let min_launcher = match raw.min_launcher.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(Version::parse(raw).map_err(|_| format!("bad minLauncher {raw:?}"))?),
    };
    for set in &raw.equivalents {
        if set.is_empty() || set.iter().any(|p| strict_relative_path(p, true).is_none()) {
            return Err("bad equivalents".into());
        }
    }
    for path in &raw.replaces {
        let ok = strict_relative_path(path, true)
            .is_some_and(|parts| !parts.last().is_some_and(|p| p.starts_with('.')));
        if !ok || protected(path) {
            return Err(format!("bad replaces {path:?}"));
        }
    }
    if raw.variants.is_empty() {
        return Err("no variants".into());
    }
    let mut variants: Vec<Variant> = Vec::with_capacity(raw.variants.len());
    for raw_variant in raw.variants {
        let variant = parse_variant(raw_variant, require_https)?;
        if variants.iter().any(|v| v.id == variant.id) {
            return Err(format!("duplicate variant {}", variant.id));
        }
        variants.push(variant);
    }
    if !variants.iter().any(|v| v.id == raw.default_variant) {
        return Err(format!(
            "defaultVariant {:?} is not a variant",
            raw.default_variant
        ));
    }
    let category = match category.as_str() {
        "" if raw.required => "essential".to_string(),
        "" => "other".to_string(),
        _ => category,
    };
    let mut requires = raw.requires;
    requires.dedup();
    Ok(Mod {
        id: raw.id,
        name,
        description,
        category,
        required: raw.required,
        enabled_by_default: raw.required || raw.enabled_by_default,
        requires,
        equivalents: raw.equivalents,
        replaces: raw
            .replaces
            .into_iter()
            .map(|p| p.trim_end_matches('/').to_string())
            .collect(),
        min_launcher,
        default_variant: raw.default_variant,
        variants,
    })
}

/// Ids of the mods that sit on a `requires` cycle (they can never install).
fn cycles(mods: &[Mod]) -> HashSet<String> {
    let requires: HashMap<&str, &[String]> = mods
        .iter()
        .map(|m| (m.id.as_str(), m.requires.as_slice()))
        .collect();
    let mut bad = HashSet::new();
    for start in mods {
        // Depth-first from `start`: reaching it again is a cycle.
        let mut stack: Vec<&str> = start.requires.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if id == start.id {
                bad.insert(start.id.clone());
                break;
            }
            if seen.insert(id) {
                if let Some(next) = requires.get(id) {
                    stack.extend(next.iter().map(String::as_str));
                }
            }
        }
    }
    bad
}

/// Parses and validates a catalog (whose signature already checked out).
/// `require_https`: package URLs must be https (release builds).
pub fn parse(text: &str, require_https: bool) -> Result<Catalog, Refused> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| Refused::Invalid(format!("not JSON: {e}")))?;
    let Some(schema) = value.get("schemaVersion").and_then(Value::as_u64) else {
        return Err(Refused::Invalid("no schemaVersion".into()));
    };
    if schema > SCHEMA_VERSION {
        return Err(Refused::TooNew(schema));
    }
    let raw: RawCatalog =
        serde_json::from_value(value).map_err(|e| Refused::Invalid(e.to_string()))?;

    let mut mods: Vec<Mod> = Vec::with_capacity(raw.mods.len());
    for (index, value) in raw.mods.into_iter().enumerate() {
        let label = value
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("#{index}"), str::to_string);
        match parse_mod(value, require_https) {
            Ok(m) if mods.iter().any(|seen| seen.id == m.id) => {
                log::warn!("mods catalog: skipped duplicate mod {label}");
            }
            Ok(m) => mods.push(m),
            Err(error) => log::warn!("mods catalog: skipped mod {label}: {error}"),
        }
    }

    // Across mods: a `replaces` that would rename another mod's files.
    let owned: Vec<(String, Vec<String>)> = mods
        .iter()
        .map(|m| (m.id.clone(), m.all_paths().collect()))
        .collect();
    mods.retain(|m| {
        let clash = m.replaces.iter().any(|r| {
            let r = r.to_ascii_lowercase();
            owned
                .iter()
                .any(|(_, paths)| paths.iter().any(|p| covers(&r, p)))
        });
        if clash {
            log::warn!(
                "mods catalog: skipped mod {}: replaces a mod's own files",
                m.id
            );
        }
        !clash
    });
    let looped = cycles(&mods);
    mods.retain(|m| {
        if looped.contains(&m.id) {
            log::warn!("mods catalog: skipped mod {}: its requires loop", m.id);
        }
        !looped.contains(&m.id)
    });

    Ok(Catalog {
        serial: raw.serial,
        generated_at: raw.generated_at.unwrap_or_default(),
        mods,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    pub fn variant(id: &str, files: Value) -> Value {
        json!({
            "id": id, "label": "", "version": "1.0", "target": "game",
            "package": { "url": format!("https://cdn.example/{id}.zip"), "bytes": 10, "sha256": SHA },
            "files": files
        })
    }

    pub fn a_mod(id: &str) -> Value {
        json!({
            "id": id, "name": id, "description": "", "category": "hud",
            "required": false, "enabledByDefault": false, "requires": [],
            "equivalents": [], "replaces": [], "defaultVariant": "default",
            "variants": [variant("default", json!([
                { "path": format!("{id}.asi"), "bytes": 1, "sha256": SHA }
            ]))]
        })
    }

    fn catalog_of(mods: Vec<Value>) -> String {
        json!({ "schemaVersion": 1, "serial": 1, "generatedAt": "x", "mods": mods }).to_string()
    }

    fn ids(text: &str) -> Vec<String> {
        parse(text, true)
            .unwrap()
            .mods
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    /// Mod `x` with `change` applied: kept?
    fn kept(change: impl FnOnce(&mut Value)) -> bool {
        let mut m = a_mod("x");
        change(&mut m);
        ids(&catalog_of(vec![a_mod("ok"), m])).contains(&"x".to_string())
    }

    #[test]
    fn parses_the_signed_fixture() {
        let catalog = parse(crate::mods::signature::tests::CATALOG_7, true).unwrap();
        assert_eq!(catalog.serial, 7);
        assert_eq!(
            catalog
                .mods
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["modloader", "asi-loader", "radar"]
        );
        let radar = catalog.get("radar").unwrap();
        assert_eq!(radar.requires, ["modloader"]);
        assert_eq!(radar.replaces, ["modloader/radar_hq", "modloader/radar_lq"]);
        let hd = radar.variant("hd").unwrap();
        assert_eq!(hd.label, "Alta definición");
        assert_eq!(hd.target, Target::Modloader);
        assert_eq!(hd.files[0].entry, "radar_sarp/radar00.txd");
        assert_eq!(hd.files[0].path, "modloader/radar_sarp/radar00.txd");
        let modloader = catalog.get("modloader").unwrap();
        assert!(modloader.required && modloader.enabled_by_default);
        assert_eq!(modloader.min_launcher, Some(Version::new(1, 2, 0)));
        assert!(modloader.default_variant().files[1].keep);
        assert_eq!(
            catalog.get("asi-loader").unwrap().equivalents,
            vec![
                vec!["vorbisFile.dll".to_string(), "vorbisHooked.dll".to_string()],
                vec!["dinput8.dll".to_string()]
            ]
        );
    }

    #[test]
    fn refuses_whole_catalogs_only_when_they_are_not_one() {
        assert_eq!(
            parse(crate::mods::signature::tests::CATALOG_FUTURE, true),
            Err(Refused::TooNew(2))
        );
        for bad in [
            "",
            "[]",
            "{}",
            r#"{"schemaVersion":"1","serial":1,"mods":[]}"#,
            r#"{"schemaVersion":1,"mods":[]}"#,
            r#"{"schemaVersion":1,"serial":-1,"mods":[]}"#,
            r#"{"schemaVersion":1,"serial":1,"mods":{}}"#,
        ] {
            assert!(
                matches!(parse(bad, true), Err(Refused::Invalid(_))),
                "{bad}"
            );
        }
        // Newer, even with a shape we don't know: "update the launcher".
        assert_eq!(
            parse(r#"{"schemaVersion":3}"#, true),
            Err(Refused::TooNew(3))
        );
        let empty = parse(r#"{"schemaVersion":1,"serial":4,"mods":[]}"#, true).unwrap();
        assert!(empty.mods.is_empty());
        assert_eq!(empty.generated_at, "");
    }

    #[test]
    fn a_bad_mod_is_skipped_not_fatal() {
        assert!(kept(|_| {}));
        for version in ["2026.10.25", "0.3.10", "1.0.0-beta+2", "r5"] {
            assert!(
                kept(|m| m["variants"][0]["version"] = json!(version)),
                "{version}"
            );
        }
        type Case = Box<dyn Fn(&mut Value)>;
        let cases: Vec<Case> = vec![
            Box::new(|m| m["id"] = json!("Bad_Id")),
            Box::new(|m| m["id"] = json!("-x")),
            Box::new(|m| m["id"] = json!("x".repeat(41))),
            Box::new(|m| m["name"] = json!("  ")),
            Box::new(|m| m["name"] = json!(5)),
            Box::new(|m| m["requires"] = json!(["x"])),
            Box::new(|m| m["requires"] = json!(["Not An Id"])),
            Box::new(|m| m["requires"] = json!(null)),
            Box::new(|m| m["minLauncher"] = json!("1.2")),
            Box::new(|m| m["minLauncher"] = json!("latest")),
            Box::new(|m| m["equivalents"] = json!([[]])),
            Box::new(|m| m["equivalents"] = json!([["../x.dll"]])),
            Box::new(|m| m["replaces"] = json!(["C:/x"])),
            Box::new(|m| m["replaces"] = json!(["gta_sa.exe"])),
            Box::new(|m| m["replaces"] = json!(["models/gta3.img"])),
            Box::new(|m| m["replaces"] = json!(["modloader"])),
            Box::new(|m| m["replaces"] = json!(["modloader/.radar_hq"])),
            Box::new(|m| m["replaces"] = json!(["x.asi"])),
            Box::new(|m| m["defaultVariant"] = json!("other")),
            Box::new(|m| m["variants"] = json!([])),
            Box::new(|m| m["variants"][0]["id"] = json!("HD")),
            Box::new(|m| m["variants"][0]["version"] = json!("")),
            Box::new(|m| m["variants"][0]["version"] = json!("1 0")),
            Box::new(|m| m["variants"][0]["version"] = json!(".1")),
            Box::new(|m| m["variants"][0]["version"] = json!("1/2")),
            Box::new(|m| m["variants"][0]["version"] = json!("1".repeat(41))),
            Box::new(|m| m["variants"][0]["version"] = json!(1)),
            Box::new(|m| m["variants"][0]["target"] = json!("system32")),
            Box::new(|m| m["variants"][0]["package"]["url"] = json!("http://cdn.example/x.zip")),
            Box::new(|m| m["variants"][0]["package"]["url"] = json!("file:///C:/x.zip")),
            Box::new(|m| m["variants"][0]["package"]["bytes"] = json!(0)),
            Box::new(|m| m["variants"][0]["package"]["sha256"] = json!("abc")),
            Box::new(|m| m["variants"][0]["files"] = json!([])),
            Box::new(|m| m["variants"][0]["files"][0]["sha256"] = json!("z".repeat(64))),
            Box::new(|m| m["variants"][0]["files"][0]["bytes"] = json!(-1)),
            Box::new(|m| {
                let files = m["variants"][0]["files"].clone();
                m["variants"] =
                    json!([variant("default", files.clone()), variant("default", files)])
            }),
        ];
        for (index, case) in cases.iter().enumerate() {
            let mut m = a_mod("x");
            case(&mut m);
            let ids = ids(&catalog_of(vec![a_mod("ok"), m.clone()]));
            assert_eq!(ids, ["ok"], "case {index}: {m}");
        }
    }

    #[test]
    fn file_paths_are_strict_and_unique() {
        let with_files = |files: Value| {
            let mut m = a_mod("x");
            m["variants"] = json!([variant("default", files)]);
            ids(&catalog_of(vec![m])).len() == 1
        };
        let file = |path: &str| json!({ "path": path, "bytes": 1, "sha256": SHA });
        assert!(with_files(json!([file("a.asi"), file("dir/b.ini")])));
        for bad in [
            "../evil.dll",
            "dir/../../evil.dll",
            "/abs.dll",
            "C:/Windows/evil.dll",
            "dir\\evil.dll",
            "CON",
            "dir/nul.ini",
            "x.dll:stream",
            "dir/",
            "",
        ] {
            assert!(!with_files(json!([file(bad)])), "{bad:?}");
        }
        // Duplicates, also by case.
        assert!(!with_files(json!([file("A.asi"), file("a.ASI")])));
        // A file that is also another file's folder.
        assert!(!with_files(json!([file("dir"), file("dir/x.ini")])));
    }

    #[test]
    fn modloader_targets_resolve_under_the_modloader_folder() {
        let mut m = a_mod("x");
        m["variants"][0]["target"] = json!("modloader");
        m["variants"][0]["files"] =
            json!([{ "path": "x/y.txd", "bytes": 1, "sha256": SHA.to_uppercase() }]);
        let catalog = parse(&catalog_of(vec![m]), true).unwrap();
        let file = &catalog.mods[0].variants[0].files[0];
        assert_eq!(
            (file.entry.as_str(), file.path.as_str()),
            ("x/y.txd", "modloader/x/y.txd")
        );
        assert_eq!(file.sha256, SHA, "hashes are normalised to lower case");
    }

    #[test]
    fn unknown_fields_and_missing_optional_ones_are_fine() {
        let catalog = json!({
            "schemaVersion": 1, "serial": 3, "future": [1, 2],
            "mods": [{
                "id": "x", "name": "X", "defaultVariant": "v", "shiny": true,
                "variants": [{
                    "id": "v", "version": "1", "target": "game", "extra": {},
                    "package": { "url": "https://cdn.example/x.zip", "bytes": 1, "sha256": SHA, "mirror": "x" },
                    "files": [{ "path": "x.asi", "bytes": 1, "sha256": SHA, "mode": 644 }]
                }]
            }]
        });
        let parsed = parse(&catalog.to_string(), true).unwrap();
        let m = &parsed.mods[0];
        assert!(!m.required && !m.enabled_by_default);
        assert_eq!(m.category, "other");
        assert!(m.requires.is_empty() && m.equivalents.is_empty() && m.replaces.is_empty());
        assert_eq!(m.min_launcher, None);
        assert_eq!(m.variants[0].label, "");
        // A required mod is always on, and lands in "essential" by default.
        let mut required = a_mod("r");
        required["required"] = json!(true);
        required["category"] = json!("");
        let parsed = parse(&catalog_of(vec![required]), true).unwrap();
        assert!(parsed.mods[0].enabled_by_default);
        assert_eq!(parsed.mods[0].category, "essential");
    }

    #[test]
    fn debug_builds_may_download_over_http() {
        let mut m = a_mod("x");
        m["variants"][0]["package"]["url"] = json!("http://127.0.0.1:9/x.zip");
        let text = catalog_of(vec![m]);
        assert!(parse(&text, true).unwrap().mods.is_empty());
        assert_eq!(parse(&text, false).unwrap().mods.len(), 1);
    }

    #[test]
    fn duplicates_loops_and_clashing_replaces_are_skipped() {
        let mut dup = a_mod("a");
        dup["name"] = json!("second");
        let mut b = a_mod("b");
        b["requires"] = json!(["c"]);
        let mut c = a_mod("c");
        c["requires"] = json!(["b"]);
        let mut d = a_mod("d");
        d["requires"] = json!(["b", "missing"]);
        // Would rename a's own file out of the way.
        let mut e = a_mod("e");
        e["replaces"] = json!(["a.asi"]);
        let catalog = parse(&catalog_of(vec![a_mod("a"), dup, b, c, d, e]), true).unwrap();
        assert_eq!(
            catalog
                .mods
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "d"],
            "d stays: an unavailable dependency only blocks it"
        );
        assert_eq!(catalog.get("a").unwrap().name, "a", "the first one wins");
    }

    #[test]
    fn validates_ids() {
        for good in ["a", "0", "asi-loader", "radar-hd-2", &"x".repeat(40)] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "-a", "A", "a_b", "a.b", "ñ", " a", &"x".repeat(41)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
