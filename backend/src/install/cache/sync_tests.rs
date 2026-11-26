//! The sync end to end against a local HTTP server (`net::test_server`),
//! always into a temp folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::package::tests::build_zip;
use super::*;
use crate::net::test_server::{Fault, TestServer};

/// Deterministic, distinct, incompressible contents (so CRC names don't
/// collide and a zip of them weighs about the same).
fn content(seed: usize, len: usize) -> Vec<u8> {
    let mut x = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn name_for(body: &[u8], seed: usize) -> String {
    let ext = if seed.is_multiple_of(2) { "dff" } else { "txd" };
    format!("0x{:X}.{ext}", crc32fast::hash(body))
}

const ZIP_PATH: &str = "/cache/135.148.128.72.7777.zip?v=1";

struct Fixture {
    server: TestServer,
    list: Arc<CacheList>,
    bodies: Vec<(String, Vec<u8>)>,
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    package_dir: PathBuf,
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn url_path(&self, name: &str) -> String {
        format!("/files/{name}")
    }

    fn temps(&self) -> Vec<String> {
        fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(TEMP_SUFFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn assert_in_sync(&self) {
        for (name, body) in &self.bodies {
            assert_eq!(&fs::read(self.path(name)).unwrap(), body, "{name}");
        }
        let temps = self.temps();
        assert!(temps.is_empty(), "temp files left: {temps:?}");
    }

    /// Requests other than the package, sorted.
    fn file_requests(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .server
            .requests()
            .into_iter()
            .map(|r| r.path)
            .filter(|p| p.starts_with("/files/"))
            .collect();
        paths.sort();
        paths
    }

    fn zip_info(&self) -> &ZipInfo {
        self.list.zip.as_ref().unwrap()
    }

    fn package_file(&self) -> PathBuf {
        self.package_dir.join(package::file_name(self.zip_info()))
    }

    fn package_files(&self) -> Vec<String> {
        fs::read_dir(&self.package_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn len_for(seed: usize) -> usize {
    match seed {
        0 => 1,
        1 => 2 * 1024 * 1024,
        _ => 1 + (seed * 7919) % 300_000,
    }
}

/// `count` files of 1 B .. ~300 KB (plus one of 2 MB), served under `/files/`.
async fn fixture(count: usize, zip_uploaded_at: Option<SystemTime>) -> Fixture {
    fixture_with(count, zip_uploaded_at, |_| false).await
}

async fn fixture_sized(
    count: usize,
    zip_uploaded_at: Option<SystemTime>,
    len: impl Fn(usize) -> usize,
) -> Fixture {
    build(count, zip_uploaded_at, |_| false, len).await
}

/// Like `fixture`; the files `in_zip` picks also go into a package served at
/// `ZIP_PATH` (with a wrapper folder, a file the list doesn't know and a
/// readme), listed as the list's `zip`.
async fn fixture_with(
    count: usize,
    zip_uploaded_at: Option<SystemTime>,
    in_zip: impl Fn(usize) -> bool,
) -> Fixture {
    build(count, zip_uploaded_at, in_zip, len_for).await
}

async fn build(
    count: usize,
    zip_uploaded_at: Option<SystemTime>,
    in_zip: impl Fn(usize) -> bool,
    len: impl Fn(usize) -> usize,
) -> Fixture {
    let server = TestServer::start().await;
    let mut bodies = Vec::new();
    for seed in 0..count {
        let body = content(seed, len(seed));
        let name = name_for(&body, seed);
        server.put(&format!("/files/{name}"), body.clone());
        bodies.push((name, body));
    }
    let zipped: Vec<(String, &[u8])> = bodies
        .iter()
        .enumerate()
        .filter(|(seed, _)| in_zip(*seed))
        .map(|(_, (name, body))| (format!("135.148.128.72.7777/{name}"), body.as_slice()))
        .collect();
    let zip = (!zipped.is_empty()).then(|| {
        let unlisted = content(999, 5000);
        let unlisted_name = name_for(&unlisted, 0);
        let mut entries: Vec<(&str, &[u8])> =
            zipped.iter().map(|(n, b)| (n.as_str(), *b)).collect();
        entries.push((&unlisted_name, &unlisted));
        entries.push(("readme.txt", b"not a model"));
        let bytes = build_zip(&entries);
        server.put(ZIP_PATH, bytes.clone());
        ZipInfo {
            url: format!("{}{ZIP_PATH}", server.base),
            bytes: bytes.len() as u64,
        }
    });
    let list = CacheList {
        base_url: format!("{}/files", server.base),
        files: bodies
            .iter()
            .map(|(name, body)| tests::entry(name, body.len() as u64))
            .collect(),
        zip_uploaded_at,
        zip: zip.filter(|_| zip_uploaded_at.is_some()),
        rejected: 0,
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cache").join("135.148.128.72.7777");
    let package_dir = package::dir(&tmp.path().join("temp"));
    fs::create_dir_all(package_dir.parent().unwrap()).unwrap();
    Fixture {
        server,
        list: Arc::new(list),
        bodies,
        _tmp: tmp,
        dir,
        package_dir,
    }
}

fn collector() -> (Reporter, Arc<Mutex<Vec<Progress>>>) {
    let events: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let sink = events.clone();
    (Arc::new(move |p| sink.lock().unwrap().push(p)), events)
}

fn client() -> Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    crate::net::http::strict_client("test")
}

fn file_limits() -> Limits {
    Limits {
        response: Duration::from_secs(5),
        stall: Duration::from_secs(5),
        retry_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(10),
        attempts: 3,
    }
}

fn job<'a>(
    f: &'a Fixture,
    client: &'a Client,
    report: &'a Reporter,
    cancel: &'a Cancel,
) -> Job<'a> {
    Job {
        client,
        report,
        cancel,
        package_dir: &f.package_dir,
        package_limits: crate::net::http::tests::fast(),
        file_limits: file_limits(),
        guard: None,
    }
}

async fn run_with(f: &Fixture, report: &Reporter) -> Summary {
    let client = client();
    let cancel = Cancel::default();
    sync(&job(f, &client, report, &cancel), f.list.clone(), &f.dir)
        .await
        .unwrap()
}

async fn run_sync(f: &Fixture) -> Summary {
    let (report, _) = collector();
    run_with(f, &report).await
}

fn set_modified(path: &Path, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[tokio::test]
async fn fresh_sync_downloads_everything_with_progress() {
    let f = fixture(40, Some(SystemTime::now())).await;
    let (report, events) = collector();
    let summary = run_with(&f, &report).await;
    assert_eq!(
        summary,
        Summary {
            downloaded: 40,
            ..Summary::default()
        }
    );
    f.assert_in_sync();

    let events = events.lock().unwrap();
    assert_eq!(events[0].phase, Phase::Preflight);
    let downloads: Vec<_> = events
        .iter()
        .filter(|p| p.phase == Phase::Download)
        .collect();
    assert_eq!(
        downloads[0].message.as_deref(),
        Some("Descargando 0 de 40 archivos...")
    );
    let last = downloads.last().unwrap();
    let total: u64 = f.bodies.iter().map(|(_, b)| b.len() as u64).sum();
    assert_eq!(
        last.message.as_deref(),
        Some("Descargando 40 de 40 archivos...")
    );
    assert_eq!(
        (last.bytes_done, last.bytes_total),
        (Some(total), Some(total))
    );
    assert_eq!(last.percent, 100.0);
    assert!(last.speed_bps.is_some());
    // Throttled: far fewer events than chunks.
    assert!(downloads.len() < 200, "{} events", downloads.len());

    // The files host never sees the launcher key.
    assert!(f
        .server
        .requests()
        .iter()
        .all(|r| !r.headers.contains_key("x-launcher-key")));
}

#[tokio::test]
async fn second_sync_fetches_only_what_is_missing() {
    let f = fixture(30, Some(SystemTime::now())).await;
    run_sync(&f).await;
    f.server.forget_requests();

    let gone: Vec<String> = f.bodies.iter().step_by(6).map(|(n, _)| n.clone()).collect();
    for name in &gone {
        fs::remove_file(f.path(name)).unwrap();
    }
    let summary = run_sync(&f).await;
    assert_eq!(summary.downloaded, gone.len());
    f.assert_in_sync();
    let mut expected: Vec<String> = gone.iter().map(|n| f.url_path(n)).collect();
    expected.sort();
    assert_eq!(f.file_requests(), expected);

    // In sync: a third run requests nothing.
    f.server.forget_requests();
    assert_eq!(run_sync(&f).await, Summary::default());
    assert!(f.server.requests().is_empty());
}

#[tokio::test]
async fn a_file_with_the_wrong_size_is_replaced() {
    let f = fixture(5, Some(SystemTime::now())).await;
    run_sync(&f).await;
    let (name, _) = &f.bodies[2];
    fs::write(f.path(name), b"truncated by a crash").unwrap();
    let summary = run_sync(&f).await;
    assert_eq!(summary.downloaded, 1);
    f.assert_in_sync();
}

#[tokio::test]
async fn a_corrupted_download_is_rejected_and_not_kept() {
    let f = fixture(6, Some(SystemTime::now())).await;
    let (bad, _) = &f.bodies[3];
    f.server.fault(&f.url_path(bad), Some(Fault::Corrupt));

    let summary = run_sync(&f).await;
    assert_eq!((summary.downloaded, summary.failed), (5, 1));
    assert!(
        !f.path(bad).exists(),
        "a file failing its CRC must not stay"
    );
    assert_eq!(
        f.server.hits(&f.url_path(bad)),
        file_limits().attempts as usize
    );

    // Truncated bodies and wrong sizes are rejected the same way.
    f.server.fault(&f.url_path(bad), Some(Fault::Truncate));
    assert_eq!(run_sync(&f).await.failed, 1);
    assert!(!f.path(bad).exists());

    // Fixed upstream: the next run fetches just that one.
    f.server.clear_faults();
    f.server.forget_requests();
    assert_eq!(run_sync(&f).await.downloaded, 1);
    assert_eq!(f.server.requests().len(), 1);
    f.assert_in_sync();
}

#[tokio::test]
async fn server_errors_are_retried_then_resumed() {
    let f = fixture(20, Some(SystemTime::now())).await;
    // Transient: fails once, the retry gets it.
    f.server
        .fault(&f.url_path(&f.bodies[0].0), Some(Fault::FailTimes(1, 503)));
    f.server
        .fault(&f.url_path(&f.bodies[1].0), Some(Fault::FailTimes(2, 500)));
    // Persistent for this run.
    let broken: Vec<String> = f.bodies[5..8].iter().map(|(n, _)| n.clone()).collect();
    for name in &broken {
        f.server.fault(&f.url_path(name), Some(Fault::Status(500)));
    }

    let summary = run_sync(&f).await;
    assert_eq!(
        (summary.downloaded, summary.failed, summary.skipped),
        (17, 3, 0)
    );
    assert!(f.path(&f.bodies[0].0).exists() && f.path(&f.bodies[1].0).exists());

    // Server fixed: resuming only fetches the three that failed.
    f.server.clear_faults();
    f.server.forget_requests();
    assert_eq!(run_sync(&f).await.downloaded, 3);
    assert_eq!(f.server.requests().len(), 3);
    f.assert_in_sync();
}

#[tokio::test]
async fn gives_up_when_the_host_is_gone() {
    let f = fixture(80, Some(SystemTime::now())).await;
    for (name, _) in &f.bodies {
        f.server.fault(&f.url_path(name), Some(Fault::Status(503)));
    }
    let summary = run_sync(&f).await;
    assert_eq!(summary.downloaded, 0);
    assert!(summary.failed >= 24, "{summary:?}");
    assert!(summary.skipped > 0, "{summary:?}");
    assert_eq!(summary.failed + summary.skipped, 80);
    assert!(!summary.cancelled);
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let f = fixture(3, Some(SystemTime::now())).await;
    let (name, _) = &f.bodies[1];
    f.server.fault(
        &f.url_path(name),
        Some(Fault::Redirect("/files/elsewhere".into())),
    );
    let summary = run_sync(&f).await;
    assert_eq!(summary.failed, 1);
    assert_eq!(f.server.hits("/files/elsewhere"), 0);
}

#[tokio::test]
async fn stale_files_are_deleted_and_the_rest_left_alone() {
    let zip = SystemTime::now() - Duration::from_secs(3600);
    let f = fixture(4, Some(zip)).await;
    run_sync(&f).await;

    let old = f.path("0xDEAD.dff");
    let fresh = f.path("0xBEEF.txd");
    let lower = f.path("0xdead.txd");
    let notes = f.path("notes.txt");
    let nested = f.path("sub");
    let leftover = f.path("0xA.dff.0badf00d.sarp-part");
    for path in [&old, &fresh, &lower, &notes, &leftover] {
        fs::write(path, b"x").unwrap();
    }
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("0xDEAD.dff"), b"x").unwrap();
    let long_ago = zip - Duration::from_secs(86_400);
    for path in [&old, &lower, &notes, &leftover, &nested.join("0xDEAD.dff")] {
        set_modified(path, long_ago);
    }
    set_modified(&fresh, zip + Duration::from_secs(60)); // the game got it after the zip

    let summary = run_sync(&f).await;
    assert_eq!((summary.deleted, summary.kept), (1, 1));
    assert!(!old.exists());
    assert!(!leftover.exists(), "interrupted-sync leftovers are cleaned");
    for path in [&fresh, &lower, &notes, &nested.join("0xDEAD.dff")] {
        assert!(path.exists(), "{} must stay", path.display());
    }
    f.assert_in_sync();
}

#[tokio::test]
async fn a_list_that_would_wipe_the_folder_deletes_nothing() {
    // A trusted list (dated, valid) that only knows 2 files, against a folder
    // of 150 older cache files: it looks incomplete, so none go.
    let zip = SystemTime::now() - Duration::from_secs(3600);
    let f = fixture(2, Some(zip)).await;
    fs::create_dir_all(&f.dir).unwrap();
    let long_ago = zip - Duration::from_secs(86_400);
    let olds: Vec<PathBuf> = (0..150)
        .map(|i| f.path(&format!("0xDEAD{i:X}.dff")))
        .collect();
    for path in &olds {
        fs::write(path, b"x").unwrap();
        set_modified(path, long_ago);
    }
    let summary = run_sync(&f).await;
    assert_eq!((summary.deleted, summary.kept), (0, 150));
    assert!(olds.iter().all(|p| p.exists()));
    // Downloads still happen.
    assert_eq!(summary.downloaded, 2);
    f.assert_in_sync();
}

#[tokio::test]
async fn untrusted_lists_never_delete() {
    for (zip, empty, rejected) in [
        (None, false, 0),
        (Some(SystemTime::now()), true, 0),
        (Some(SystemTime::now()), false, 1),
    ] {
        let mut f = fixture(2, zip).await;
        fs::create_dir_all(&f.dir).unwrap();
        let old = f.path("0xDEAD.dff");
        fs::write(&old, b"x").unwrap();
        set_modified(&old, SystemTime::now() - Duration::from_secs(10 * 86_400));
        {
            let list = Arc::get_mut(&mut f.list).unwrap();
            list.rejected = rejected;
            if empty {
                list.files.clear();
            }
        }
        let summary = run_sync(&f).await;
        assert_eq!(
            (summary.deleted, summary.kept),
            (0, 1),
            "{zip:?} {empty} {rejected}"
        );
        assert!(old.exists());
    }
}

// ---- the package ----

/// 30 files; the first 26 are in the package, the last 4 (custom skins) only
/// file by file.
async fn packaged(count: usize) -> Fixture {
    fixture_with(count, Some(SystemTime::now()), move |seed| seed < count - 4).await
}

#[tokio::test]
async fn far_behind_takes_the_package_then_only_the_rest() {
    let f = packaged(30).await;
    let (report, events) = collector();
    let summary = run_with(&f, &report).await;
    assert_eq!(
        summary,
        Summary {
            extracted: 26,
            downloaded: 4,
            package: true,
            ..Summary::default()
        }
    );
    f.assert_in_sync();
    // One package request, then exactly the four files it lacked.
    assert_eq!(f.server.hits(ZIP_PATH), 1);
    let mut skins: Vec<String> = f.bodies[26..].iter().map(|(n, _)| f.url_path(n)).collect();
    skins.sort();
    assert_eq!(f.file_requests(), skins);
    // Nothing the list doesn't know was unpacked, and the package is gone.
    let on_disk = fs::read_dir(&f.dir).unwrap().count();
    assert_eq!(on_disk, 30);
    assert!(!f.package_dir.exists(), "{:?}", f.package_files());

    // The phases, in order: package download, unpack, the rest.
    let events = events.lock().unwrap().clone();
    let steps: Vec<(Phase, String)> = events
        .iter()
        .map(|p| {
            let message = p.message.clone().unwrap_or_default();
            let step = if message.starts_with("Descargando paquete") {
                "package"
            } else if message.starts_with("Extrayendo") {
                "extract"
            } else if message.contains("archivos restantes") {
                "rest"
            } else {
                "other"
            };
            (p.phase, step.to_string())
        })
        .collect();
    let mut order: Vec<&str> = Vec::new();
    for (_, step) in &steps {
        if step != "other" && order.last() != Some(&step.as_str()) {
            order.push(step);
        }
    }
    assert_eq!(order, vec!["package", "extract", "rest"], "{steps:?}");
    let last_package = events
        .iter()
        .rfind(|p| p.message.as_deref() == Some(PACKAGE_MESSAGE))
        .unwrap();
    assert_eq!(last_package.bytes_done, Some(f.zip_info().bytes));
    assert!(events.iter().any(
        |p| p.message.as_deref() == Some("Extrayendo 26 de 26 archivos...") && p.percent == 100.0
    ));
    assert!(events
        .iter()
        .any(|p| p.message.as_deref() == Some("Descargando 4 de 4 archivos restantes...")));

    // In sync: the next run downloads nothing at all.
    f.server.forget_requests();
    assert_eq!(run_sync(&f).await, Summary::default());
    assert!(f.server.requests().is_empty());
}

#[tokio::test]
async fn nearly_complete_goes_file_by_file_without_touching_the_package() {
    let f = packaged(30).await;
    run_sync(&f).await;
    f.server.forget_requests();
    // Two small files gone: far less than the package weighs.
    let gone = [&f.bodies[4].0, &f.bodies[27].0];
    for name in gone {
        fs::remove_file(f.path(name)).unwrap();
    }
    let summary = run_sync(&f).await;
    assert_eq!((summary.downloaded, summary.package), (2, false));
    assert_eq!(f.server.hits(ZIP_PATH), 0);
    let mut expected: Vec<String> = gone.iter().map(|n| f.url_path(n)).collect();
    expected.sort();
    assert_eq!(f.file_requests(), expected);
    f.assert_in_sync();
}

#[tokio::test]
async fn the_package_resumes_after_a_drop_without_downloading_twice() {
    let f = packaged(30).await;
    let size = f.zip_info().bytes as usize;
    let cut = size / 3;
    f.server.fault(ZIP_PATH, Some(Fault::DropTimes(1, cut)));
    let (report, events) = collector();
    let summary = run_with(&f, &report).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    f.assert_in_sync();

    let requests: Vec<_> = f
        .server
        .requests()
        .into_iter()
        .filter(|r| r.path == ZIP_PATH)
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers.get("range"), None);
    assert_eq!(
        requests[1].headers.get("range").map(String::as_str),
        Some(format!("bytes={cut}-").as_str())
    );
    // Every byte crossed the wire once.
    assert_eq!(f.server.sent(ZIP_PATH), size);
    // The progress after the drop continued from where it was, not from 0.
    let events = events.lock().unwrap();
    let package: Vec<u64> = events
        .iter()
        .filter(|p| p.phase == Phase::Download && p.bytes_total == Some(size as u64))
        .filter_map(|p| p.bytes_done)
        .collect();
    let after_retry = events
        .iter()
        .position(|p| p.message.as_deref() == Some("Se cortó la conexión. Reintentando..."))
        .expect("a retry message");
    assert_eq!(events[after_retry].bytes_done, Some(cut as u64));
    assert!(package.windows(2).all(|w| w[0] <= w[1]), "{package:?}");
}

/// The package as the server has it.
async fn zip_bytes(f: &Fixture) -> Vec<u8> {
    let bytes = client()
        .get(format!("{}{ZIP_PATH}", f.server.base))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    f.server.forget_requests();
    bytes
}

#[tokio::test]
async fn the_package_resumes_across_a_restart() {
    let f = packaged(30).await;
    // A previous launcher run got this far and was closed.
    let zip_bytes = zip_bytes(&f).await;
    let kept = zip_bytes.len() / 2;
    fs::create_dir_all(&f.package_dir).unwrap();
    fs::write(f.package_file(), &zip_bytes[..kept]).unwrap();

    let summary = run_sync(&f).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    f.assert_in_sync();
    let request = f
        .server
        .requests()
        .into_iter()
        .find(|r| r.path == ZIP_PATH)
        .unwrap();
    assert_eq!(
        request.headers.get("range").map(String::as_str),
        Some(format!("bytes={kept}-").as_str())
    );
    assert_eq!(f.server.sent(ZIP_PATH), zip_bytes.len() - kept);
}

#[tokio::test]
async fn a_finished_package_on_disk_is_unpacked_without_a_request() {
    let f = packaged(30).await;
    let zip_bytes = zip_bytes(&f).await;
    fs::create_dir_all(&f.package_dir).unwrap();
    fs::write(f.package_file(), &zip_bytes).unwrap();
    let summary = run_sync(&f).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    assert_eq!(f.server.hits(ZIP_PATH), 0);
    f.assert_in_sync();
}

#[tokio::test]
async fn a_new_package_version_discards_the_old_partial() {
    let f = packaged(30).await;
    // A partial of the previous upload (other `?v=`) and some junk.
    let old = ZipInfo {
        url: f.zip_info().url.replace("?v=1", "?v=0"),
        bytes: f.zip_info().bytes,
    };
    fs::create_dir_all(&f.package_dir).unwrap();
    let old_file = f.package_dir.join(package::file_name(&old));
    fs::write(&old_file, vec![0xAB; 1000]).unwrap();
    fs::write(f.package_dir.join("leftover.tmp"), b"x").unwrap();
    assert_eq!(package::on_disk(&f.package_dir, f.zip_info()), 0);

    let summary = run_sync(&f).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    f.assert_in_sync();
    assert!(!old_file.exists());
    let request = f
        .server
        .requests()
        .into_iter()
        .find(|r| r.path == ZIP_PATH)
        .unwrap();
    assert_eq!(request.headers.get("range"), None, "from the start");
    assert!(!f.package_dir.exists());
}

#[tokio::test]
async fn a_server_that_ignores_range_restarts_cleanly() {
    let f = packaged(30).await;
    let zip_bytes = zip_bytes(&f).await;
    // A partial with garbage at the end: if it were appended to, the zip
    // would be corrupt.
    let mut partial = zip_bytes[..zip_bytes.len() / 2].to_vec();
    partial.extend_from_slice(&[0xEE; 64]);
    fs::create_dir_all(&f.package_dir).unwrap();
    fs::write(f.package_file(), &partial).unwrap();
    f.server.fault(ZIP_PATH, Some(Fault::IgnoreRange));

    let summary = run_sync(&f).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    f.assert_in_sync();
    assert_eq!(f.server.hits(ZIP_PATH), 1);
}

#[tokio::test]
async fn a_corrupt_package_falls_back_to_file_by_file() {
    let f = packaged(30).await;
    // Same size, but not a zip.
    let size = f.zip_info().bytes as usize;
    f.server.put(ZIP_PATH, content(4242, size));
    let summary = run_sync(&f).await;
    assert_eq!(
        (summary.extracted, summary.downloaded, summary.package),
        (0, 30, true)
    );
    f.assert_in_sync();
    // The bad package is not kept for the next run.
    assert!(!f.package_dir.exists(), "{:?}", f.package_files());
}

#[tokio::test]
async fn a_package_that_cannot_be_downloaded_falls_back_to_file_by_file() {
    for fault in [
        Fault::Status(404),
        Fault::Status(503),
        Fault::Redirect("/x".into()),
    ] {
        let f = packaged(30).await;
        f.server.fault(ZIP_PATH, Some(fault.clone()));
        let summary = run_sync(&f).await;
        assert_eq!(
            (summary.extracted, summary.downloaded, summary.failed),
            (0, 30, 0),
            "{fault:?}"
        );
        f.assert_in_sync();
        assert_eq!(f.server.hits("/x"), 0);
    }
}

#[tokio::test]
async fn an_old_api_without_zip_goes_file_by_file() {
    let mut f = packaged(30).await;
    Arc::get_mut(&mut f.list).unwrap().zip = None;
    // An old partial is useless now.
    fs::create_dir_all(&f.package_dir).unwrap();
    fs::write(f.package_dir.join("cache-0123456789abcdef.zip"), b"old").unwrap();
    let summary = run_sync(&f).await;
    assert_eq!((summary.downloaded, summary.package), (30, false));
    assert_eq!(f.server.hits(ZIP_PATH), 0);
    assert!(!f.package_dir.exists());
    f.assert_in_sync();
}

// ---- cancel and bad lines ----

#[tokio::test]
async fn cancel_mid_package_keeps_the_partial_and_a_rerun_resumes() {
    let f = packaged(30).await;
    let size = f.zip_info().bytes as usize;
    let half = size / 2;
    // Half the package, then the line hangs (longer than the test).
    f.server
        .fault(ZIP_PATH, Some(Fault::StallTimes(1, half, 60_000)));
    let client = client();
    let cancel = Cancel::default();
    let (report, _) = collector();
    let job = Job {
        // A stall timeout that would never fire during the test.
        package_limits: Limits {
            stall: Duration::from_secs(120),
            ..crate::net::http::tests::fast()
        },
        ..job(&f, &client, &report, &cancel)
    };
    let started = std::time::Instant::now();
    // Cancelled once part of it is on disk (the writer flushes every 256 KB).
    let canceller = async {
        while package::on_disk(&f.package_dir, f.zip_info()) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
        std::time::Instant::now()
    };
    let (summary, cancelled_at) = tokio::join!(sync(&job, f.list.clone(), &f.dir), canceller);
    let summary = summary.unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "stopped promptly"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(summary.cancelled);
    assert_eq!((summary.extracted, summary.downloaded), (0, 0));
    assert_eq!(summary.missing(), 30);
    assert!(f.temps().is_empty());
    // The partial stays, with everything that arrived (flushed on cancel).
    let kept = package::on_disk(&f.package_dir, f.zip_info()) as usize;
    assert!((1..=half).contains(&kept), "{kept} of {half}");

    // Next run: continues from there, every byte sent once.
    f.server.forget_requests();
    let summary = run_sync(&f).await;
    assert_eq!((summary.extracted, summary.downloaded), (26, 4));
    f.assert_in_sync();
    let request = f
        .server
        .requests()
        .into_iter()
        .find(|r| r.path == ZIP_PATH)
        .unwrap();
    assert_eq!(
        request.headers.get("range").map(String::as_str),
        Some(format!("bytes={kept}-").as_str())
    );
    assert_eq!(f.server.sent(ZIP_PATH), size - kept);
}

#[tokio::test]
async fn cancel_mid_file_by_file_leaves_no_temp_and_a_rerun_continues() {
    let f = fixture(40, Some(SystemTime::now())).await;
    // Every file trickles in, so plenty are in flight when it stops.
    for (name, _) in &f.bodies {
        f.server
            .fault(&f.url_path(name), Some(Fault::Trickle(8 * 1024, 15)));
    }
    let client = client();
    let cancel = Cancel::default();
    let (report, _) = collector();
    let job = job(&f, &client, &report, &cancel);
    let canceller = async {
        loop {
            let done = fs::read_dir(&f.dir)
                .map(|d| {
                    d.flatten()
                        .filter(|e| is_cache_name(&e.file_name().to_string_lossy()))
                        .count()
                })
                .unwrap_or(0);
            if done >= 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
        std::time::Instant::now()
    };
    let (summary, cancelled_at) = tokio::join!(sync(&job, f.list.clone(), &f.dir), canceller);
    let summary = summary.unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "stopped promptly"
    );
    assert!(summary.cancelled);
    assert!(
        summary.downloaded >= 5 && summary.downloaded < 40,
        "{summary:?}"
    );
    assert_eq!(summary.failed, 0, "cancelled files are not failures");
    assert_eq!(summary.downloaded + summary.skipped, 40);
    assert!(f.temps().is_empty(), "{:?}", f.temps());
    // In parallel, never more than the top pace.
    let most = f.server.max_in_flight();
    assert!((2..=12).contains(&most), "{most} at once");

    f.server.clear_faults();
    f.server.forget_requests();
    let rerun = run_sync(&f).await;
    assert_eq!(rerun.downloaded, 40 - summary.downloaded);
    assert_eq!(f.file_requests().len(), 40 - summary.downloaded);
    f.assert_in_sync();
}

#[tokio::test]
async fn a_bad_line_slows_down_instead_of_giving_up() {
    // One slow line for everything: 16 KB pieces at 640 KB/s, 25 ms each.
    // Twelve files at once get a piece every 300 ms each, longer than the
    // 200 ms stall timeout: at the old fixed pace every attempt stalls and
    // 24 failed files in a row end the sync. Six at once (150 ms) is fine.
    let f = fixture_sized(36, Some(SystemTime::now()), |seed| 20_000 + seed * 500).await;
    f.server.line(640 * 1024, 16 * 1024);
    let client = client();
    let cancel = Cancel::default();
    let (report, _) = collector();
    let job = Job {
        file_limits: Limits {
            stall: Duration::from_millis(200),
            ..file_limits()
        },
        ..job(&f, &client, &report, &cancel)
    };
    let summary = sync(&job, f.list.clone(), &f.dir).await.unwrap();
    assert_eq!(
        (summary.downloaded, summary.failed, summary.skipped),
        (36, 0, 0),
        "{summary:?}"
    );
    f.assert_in_sync();
    // The line did stall: some attempts were repeated.
    let requests = f.file_requests().len();
    assert!(requests > 36, "{requests} requests");
}

#[tokio::test]
async fn a_slow_file_longer_than_any_cap_still_arrives() {
    let f = fixture(3, Some(SystemTime::now())).await;
    let (big, body) = &f.bodies[1];
    assert_eq!(body.len(), 2 * 1024 * 1024);
    // 2 MB at 16 KB every 30 ms: ~3.8 s, over twice a 1.5 s stall timeout
    // (and each gap a fiftieth of it).
    f.server
        .fault(&f.url_path(big), Some(Fault::Trickle(16 * 1024, 30)));
    let client = client();
    let cancel = Cancel::default();
    let (report, _) = collector();
    let job = Job {
        file_limits: Limits {
            stall: Duration::from_millis(1500),
            ..file_limits()
        },
        ..job(&f, &client, &report, &cancel)
    };
    let started = std::time::Instant::now();
    let summary = sync(&job, f.list.clone(), &f.dir).await.unwrap();
    assert!(started.elapsed() > Duration::from_secs(3));
    assert_eq!(summary.downloaded, 3);
    assert_eq!(f.server.hits(&f.url_path(big)), 1);
    f.assert_in_sync();
}

// ---- the list from the API ----

#[tokio::test]
async fn list_comes_from_the_api_with_the_key_and_is_cached() {
    let server = TestServer::start().await;
    let body = content(7, 1234);
    let name = name_for(&body, 0);
    server.put(&format!("/files/{name}"), body.clone());
    server.put(
        api::CACHE,
        serde_json::to_vec(&json!({
            "message": "OK", "httpCode": 200, "source": "launcher",
            "payload": {
                "baseUrl": format!("{}/files", server.base),
                "zipUploadedAt": "2026-11-22T23:50:57.000Z",
                "count": 1, "totalBytes": 1234,
                "files": [[name, 1234]],
                "zip": { "url": format!("{}/cache.zip?v=9", server.base), "bytes": 99_999 }
            }
        }))
        .unwrap(),
    );
    let api = Api::new(client(), server.base.clone());
    let state = CacheState::default();
    let list = state.list(&api).await.unwrap();
    assert_eq!(list.files, vec![tests::entry(&name, 1234)]);
    assert!(list.may_delete());
    assert_eq!(
        list.zip,
        Some(ZipInfo {
            url: format!("{}/cache.zip?v=9", server.base),
            bytes: 99_999
        })
    );
    // Within the TTL the health check and the sync share one request.
    state.list(&api).await.unwrap();
    assert_eq!(server.hits(api::CACHE), 1);

    let tmp = tempfile::tempdir().unwrap();
    let (report, _) = collector();
    let cancel = Cancel::default();
    let package_dir = package::dir(tmp.path());
    let job = Job::new(api.client(), &report, &cancel, &package_dir, None);
    let summary = sync(&job, list, &tmp.path().join("cache")).await.unwrap();
    // 1234 bytes missing against a 99,999-byte zip: file by file.
    assert_eq!((summary.downloaded, summary.package), (1, false));
    assert_eq!(
        fs::read(tmp.path().join("cache").join(&name)).unwrap(),
        body
    );

    let requests = server.requests();
    let key_of = |path: &str| {
        requests
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.headers.contains_key("x-launcher-key"))
    };
    assert_eq!(key_of(api::CACHE), Some(true));
    assert_eq!(key_of(&format!("/files/{name}")), Some(false));
}

#[tokio::test]
async fn the_package_never_carries_the_key() {
    let f = packaged(30).await;
    run_sync(&f).await;
    let zip_request = f
        .server
        .requests()
        .into_iter()
        .find(|r| r.path == ZIP_PATH)
        .unwrap();
    assert!(!zip_request.headers.contains_key("x-launcher-key"));
}

#[tokio::test]
async fn a_slow_list_still_arrives() {
    let server = TestServer::start().await;
    let files: Vec<_> = (0..3000)
        .map(|i| json!([format!("0x{:X}.dff", i + 1), 10]))
        .collect();
    let body = serde_json::to_vec(&json!({
        "message": "OK", "httpCode": 200, "source": "launcher",
        "payload": {
            "baseUrl": format!("{}/files", server.base),
            "zipUploadedAt": "2026-11-22T23:50:57.000Z",
            "files": files
        }
    }))
    .unwrap();
    server.put(api::CACHE, body);
    // ~60 KB in 4 KB pieces every 30 ms: ~0.5 s, longer than the 0.3 s the
    // response may take. What arrives keeps the request alive.
    server.fault(api::CACHE, Some(Fault::Trickle(4096, 30)));
    let api = Api::new(client(), server.base.clone());
    let value = api
        .get(api::CACHE, Duration::from_millis(300))
        .await
        .unwrap();
    assert_eq!(parse_list(value, false).unwrap().files.len(), 3000);
}

#[tokio::test]
async fn api_errors_are_not_cached_and_never_reach_the_folder() {
    let server = TestServer::start().await;
    server.fault(api::CACHE, Some(Fault::Status(500)));
    let api = Api::new(client(), server.base.clone());
    let state = CacheState::default();
    assert!(state.list(&api).await.is_err());
    assert!(state.list(&api).await.is_err());
    assert_eq!(server.hits(api::CACHE), 2);

    server.fault(api::CACHE, Some(Fault::Status(401)));
    assert_eq!(
        state.list(&api).await.unwrap_err(),
        "El servidor respondió con un error (HTTP 401)."
    );
}

#[tokio::test]
async fn rwguard_validates_existing_cache_and_blocks_unsafe_downloads_concurrently() {
    use crate::rwguard::history::{self, RwGuardHistory};
    use crate::rwguard::scanner::rw;
    use crate::rwguard::scanner::tests::{build_dff, build_txd};

    let server = TestServer::start().await;
    let existing_good = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX);
    let download_good = build_txd(4, 16, 16, 16, 0x08, rw::D3DFMT_DXT1, 128);
    let download_bad = build_dff(rw::ATOMIC, rw::ATOMIC_STRUCT_MAX + 64);

    let name_existing = format!("0x{:X}.dff", crc32fast::hash(&existing_good));
    let name_good = format!("0x{:X}.txd", crc32fast::hash(&download_good));
    let name_bad = format!("0x{:X}.dff", crc32fast::hash(&download_bad));

    server.put(&format!("/files/{name_existing}"), existing_good.clone());
    server.put(&format!("/files/{name_good}"), download_good.clone());
    server.put(&format!("/files/{name_bad}"), download_bad.clone());

    let list = Arc::new(CacheList {
        base_url: format!("{}/files", server.base),
        files: vec![
            tests::entry(&name_existing, existing_good.len() as u64),
            tests::entry(&name_good, download_good.len() as u64),
            tests::entry(&name_bad, download_bad.len() as u64),
        ],
        zip_uploaded_at: Some(SystemTime::now()),
        zip: None,
        rejected: 0,
    });

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cache");
    let package_dir = package::dir(&tmp.path().join("temp"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(&name_existing), &existing_good).unwrap();

    let hist = Arc::new(RwGuardHistory::open(tmp.path().join(history::FILE_NAME)));
    let guard = Arc::new(CacheGuard::new(hist.clone(), &dir, Arc::new(|_| {})));
    let (report, _) = collector();
    let client = client();
    let cancel = Cancel::default();
    let job = Job {
        client: &client,
        report: &report,
        cancel: &cancel,
        package_dir: &package_dir,
        package_limits: crate::net::http::tests::fast(),
        file_limits: file_limits(),
        guard: Some(guard.clone()),
    };

    let summary = sync(&job, list, &dir).await.unwrap();
    let (reused, newly, live, blocked) = guard.finish();

    assert_eq!(summary.downloaded, 1);
    assert_eq!(summary.blocked, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!((reused, newly, live, blocked), (0, 2, 1, 1));
    assert!(dir.join(&name_good).exists());
    assert!(
        !dir.join(&name_bad).exists(),
        "unsafe model must not land in cache"
    );
    assert_eq!(
        server.hits(&format!("/files/{name_bad}")),
        1,
        "blocked models are not retried"
    );

    // Next incremental scan reuses both validated models with zero new scans.
    let follow = rwguard::scan_dir_incremental(&hist, &dir, false, None).unwrap();
    assert_eq!(follow.reused, 2);
    assert_eq!(follow.newly_scanned, 0);
}

// ---- Contract test against the running API (`cargo test -- --ignored`).

#[tokio::test]
#[ignore = "needs the API"]
async fn real_cache_list_parses() {
    let api = crate::net::api::tests::real_api();
    let list = fetch_list(&api).await.unwrap();
    assert!(list.base_url.starts_with("https://"), "{}", list.base_url);
    assert_eq!(list.rejected, 0);
    eprintln!(
        "real list: {} files, zipUploadedAt {:?}, zip {:?}",
        list.files.len(),
        list.zip_uploaded_at,
        list.zip
    );
}
