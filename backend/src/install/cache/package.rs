//! The cache package: the server's whole cache as one zip on the CDN (`zip`
//! in the list). A player far behind downloads it (resumable, also across
//! restarts) and unpacks only the listed files they lack; the file-by-file
//! sync then fetches whatever is still missing.
//!
//! The download lives in the temp folder, never in the cache folder: the
//! game must not see a half-written package, and Documents is often synced
//! by OneDrive.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::{temp_name, with_writable, Entry, ZipInfo, PROGRESS_THROTTLE};
use crate::install::cancel::Cancel;
use crate::install::progress::{percent, Phase, Progress, Reporter};
use crate::rwguard::CacheGuard;

/// Under `%TEMP%`.
pub const DIR_NAME: &str = "sarp-cache-zip";
/// A package nothing wrote to for this long was abandoned: swept at startup.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const READ_CHUNK: usize = 64 * 1024;
const WRITE_BUFFER: usize = 64 * 1024;

pub fn dir(temp_root: &Path) -> PathBuf {
    temp_root.join(DIR_NAME)
}

/// `cache-<hash>.zip`, hashed from URL and size: a new upload (another
/// `?v=`) gets another name, so a partial of the old one is never resumed
/// into it.
pub fn file_name(zip: &ZipInfo) -> String {
    let hash = Sha256::digest(format!("{}\n{}", zip.url, zip.bytes).as_bytes());
    format!("cache-{}.zip", &hex::encode(hash)[..16])
}

/// Bytes of `zip` already in `dir`.
pub fn on_disk(dir: &Path, zip: &ZipInfo) -> u64 {
    fs::symlink_metadata(dir.join(file_name(zip)))
        .ok()
        .filter(|meta| meta.file_type().is_file() && meta.len() <= zip.bytes)
        .map_or(0, |meta| meta.len())
}

/// Deletes every file in `dir` except `keep` (older uploads, a finished
/// package), and `dir` itself once empty. Plain files only: links and
/// folders are never followed or removed.
pub fn prune(dir: &Path, keep: Option<&str>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if keep.is_some_and(|keep| entry.file_name() == keep) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => log::info!("cache package: removed {}", entry.path().display()),
            Err(error) => log::warn!("could not delete {}: {error}", entry.path().display()),
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Startup: a package folder with nothing written for a week goes.
pub fn sweep(temp_root: &Path) {
    let dir = dir(temp_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let newest = entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max();
    let abandoned = match (newest, SystemTime::now().checked_sub(KEEP)) {
        (None, _) => true,
        (Some(newest), Some(cutoff)) => newest < cutoff,
        (Some(_), None) => false,
    };
    if abandoned {
        prune(&dir, None);
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Unpacked {
    /// Missing files now in place.
    pub written: usize,
    /// Picked entries that did not make it (read error, size or CRC not
    /// matching the name, rename refused). File by file gets them.
    pub bad: usize,
    /// Picked entries rejected by RWGuard before landing in the cache folder.
    pub blocked: usize,
}

/// The base name of a zip entry (`<wrapper>/0xA.dff` → `0xA.dff`). Nothing
/// else of the path is ever used, so no entry can land outside the folder.
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or_default()
}

/// Progress over the picked entries, by uncompressed bytes.
struct Meter<'a> {
    report: &'a Reporter,
    files: usize,
    bytes: u64,
    files_done: usize,
    bytes_done: u64,
    last_emit: Option<Instant>,
}

impl Meter<'_> {
    fn emit(&mut self, force: bool) {
        let now = Instant::now();
        if !force
            && self
                .last_emit
                .is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE)
        {
            return;
        }
        self.last_emit = Some(now);
        (self.report)(
            Progress::new(Phase::Extract, percent(self.bytes_done, self.bytes))
                .bytes(self.bytes_done, self.bytes)
                .message(format!(
                    "Extrayendo {} de {} archivos...",
                    self.files_done, self.files
                )),
        );
    }
}

/// Unpacks into `dir` the entries of `zip` whose base name is one of the
/// `missing` files, with that file's size and CRC. Everything else in the
/// zip is ignored: other names, files the folder already has, files the
/// list doesn't know. Each one is written to a temp name, its CRC checked
/// while streaming, and renamed into place. Blocking.
///
/// `Err` only when the zip can't be read at all. On cancel it stops after
/// the current chunk and returns what it wrote.
pub fn extract(
    zip: &Path,
    dir: &Path,
    missing: &[Entry],
    report: &Reporter,
    cancel: &Cancel,
    guard: Option<&CacheGuard>,
) -> Result<Unpacked, String> {
    let file = File::open(zip).map_err(|e| format!("open: {e}"))?;
    let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| format!("zip: {e}"))?;

    let wanted: HashMap<&str, &Entry> = missing.iter().map(|e| (e.name.as_str(), e)).collect();
    let mut taken: HashSet<&str> = HashSet::new();
    let mut picked: Vec<(usize, &Entry)> = Vec::new();
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index_data(index) else {
            continue;
        };
        let Ok(name) = entry.name() else { continue };
        let Some(&target) = wanted.get(base_name(&name)) else {
            continue;
        };
        // Same size and CRC as the name promises, and only once.
        if entry.is_file()
            && entry.size() == target.size
            && entry.crc32() == target.crc
            && taken.insert(target.name.as_str())
        {
            picked.push((index, target));
        }
    }

    let mut meter = Meter {
        report,
        files: picked.len(),
        bytes: picked.iter().map(|(_, e)| e.size).sum(),
        files_done: 0,
        bytes_done: 0,
        last_emit: None,
    };
    meter.emit(true);
    let mut unpacked = Unpacked::default();
    let mut buf = vec![0u8; READ_CHUNK];
    for (index, target) in picked {
        if cancel.is_cancelled() {
            break;
        }
        let before = meter.bytes_done;
        match unpack(
            &mut archive,
            index,
            target,
            dir,
            &mut buf,
            cancel,
            &mut meter,
            guard,
        ) {
            Ok(()) => unpacked.written += 1,
            Err(UnpackErr::Cancelled) => break,
            Err(UnpackErr::Blocked(reason)) => {
                log::warn!(
                    "cache package: {} blocked by RWGuard: {reason}",
                    target.name
                );
                unpacked.blocked += 1;
            }
            Err(UnpackErr::Other(error)) => {
                log::warn!("cache package: {} not unpacked: {error}", target.name);
                unpacked.bad += 1;
            }
        }
        meter.bytes_done = before + target.size;
        meter.files_done += 1;
        meter.emit(false);
    }
    meter.emit(true);
    Ok(unpacked)
}

enum UnpackErr {
    Cancelled,
    Blocked(String),
    Other(String),
}

#[allow(clippy::too_many_arguments)]
fn unpack<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    target: &Entry,
    dir: &Path,
    buf: &mut [u8],
    cancel: &Cancel,
    meter: &mut Meter<'_>,
    guard: Option<&CacheGuard>,
) -> Result<(), UnpackErr> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| UnpackErr::Other(e.to_string()))?;
    let temp = dir.join(temp_name(&target.name));
    let result = (|| {
        let io = |e: io::Error| UnpackErr::Other(e.to_string());
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(io)?;
        let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
        let mut crc = crc32fast::Hasher::new();
        let mut written = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(UnpackErr::Cancelled);
            }
            let n = entry
                .read(buf)
                .map_err(|e| UnpackErr::Other(format!("inflate: {e}")))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > target.size {
                return Err(UnpackErr::Other(format!(
                    "more than the listed {} bytes",
                    target.size
                )));
            }
            crc.update(&buf[..n]);
            writer.write_all(&buf[..n]).map_err(io)?;
            meter.bytes_done += n as u64;
            meter.emit(false);
        }
        writer.flush().map_err(io)?;
        drop(writer);
        if written != target.size {
            return Err(UnpackErr::Other(format!(
                "{written} of {} bytes",
                target.size
            )));
        }
        let actual = crc.finalize();
        if actual != target.crc {
            return Err(UnpackErr::Other(format!(
                "content CRC {actual:X} does not match the name"
            )));
        }
        if let Some(guard) = guard {
            guard
                .validate_temp(&temp, &target.name, target.size)
                .map_err(UnpackErr::Blocked)?;
        }
        let path = dir.join(&target.name);
        with_writable(&path, || fs::rename(&temp, &path))
            .map_err(|e| UnpackErr::Other(format!("rename: {e}")))?;
        if let Some(guard) = guard {
            guard.record_landed(&path, &target.name, target.size);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::tests::entry;
    use super::*;
    use std::sync::{Arc, Mutex};
    use zip::write::SimpleFileOptions;

    /// Writes a zip of `(entry name, content)` pairs.
    pub fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                zip.start_file(*name, options).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        out.into_inner()
    }

    fn named(body: &[u8], ext: &str) -> String {
        format!("0x{:X}.{ext}", crc32fast::hash(body))
    }

    fn quiet() -> Reporter {
        Arc::new(|_| {})
    }

    #[test]
    fn names_are_tied_to_the_upload() {
        let a = ZipInfo {
            url: "https://cdn.example/cache/x.zip?v=1".into(),
            bytes: 10,
        };
        let b = ZipInfo {
            url: "https://cdn.example/cache/x.zip?v=2".into(),
            ..a.clone()
        };
        let c = ZipInfo {
            bytes: 11,
            ..a.clone()
        };
        assert_eq!(file_name(&a), file_name(&a.clone()));
        assert_ne!(file_name(&a), file_name(&b));
        assert_ne!(file_name(&a), file_name(&c));
        assert!(file_name(&a).starts_with("cache-") && file_name(&a).ends_with(".zip"));
        assert_eq!(file_name(&a).len(), "cache-".len() + 16 + ".zip".len());
    }

    #[test]
    fn base_names_drop_every_folder() {
        assert_eq!(base_name("0xA.dff"), "0xA.dff");
        assert_eq!(base_name("cache/135.148.128.72.7777/0xA.dff"), "0xA.dff");
        assert_eq!(base_name("..\\..\\0xA.dff"), "0xA.dff");
        assert_eq!(base_name("C:/Windows/0xA.dff"), "0xA.dff");
        assert_eq!(base_name("folder/"), "");
    }

    #[test]
    fn prune_and_sweep_keep_what_they_should() {
        let root = tempfile::tempdir().unwrap();
        let dir = dir(root.path());
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("cache-old.zip"), b"old").unwrap();
        fs::write(dir.join("cache-new.zip"), b"new").unwrap();
        prune(&dir, Some("cache-new.zip"));
        assert!(!dir.join("cache-old.zip").exists());
        assert!(dir.join("cache-new.zip").exists());
        assert!(dir.join("nested").is_dir(), "folders are left alone");

        // Fresh: the sweep keeps it.
        sweep(root.path());
        assert!(dir.join("cache-new.zip").exists());
        // Untouched for over a week: gone.
        let old = SystemTime::now() - KEEP - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(dir.join("cache-new.zip"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::remove_dir(dir.join("nested")).unwrap();
        sweep(root.path());
        assert!(!dir.exists());
        // Nothing there: nothing happens.
        sweep(root.path());
    }

    #[test]
    fn extracts_only_missing_listed_files_flat_and_verified() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        let outside = root.path().join("outside");

        let a = b"model a".repeat(500);
        let b = b"texture b".repeat(300);
        let c = b"model c".repeat(10);
        let d = b"model d, already in the folder".to_vec();
        let e = b"not in the list".to_vec();
        let good_crc_wrong_body = b"pretends to be f".to_vec();
        let f = b"the real f".to_vec();
        let (na, nb, nc, nd, ne, nf) = (
            named(&a, "dff"),
            named(&b, "txd"),
            named(&c, "dff"),
            named(&d, "dff"),
            named(&e, "dff"),
            named(&f, "txd"),
        );
        fs::write(cache.join(&nd), &d).unwrap();
        let zip_bytes = build_zip(&[
            // Wrapper folder: lands flat.
            (&format!("135.148.128.72.7777/{na}"), &a),
            // Traversal and absolute names: only the base name is used.
            (&format!("../../outside/{nb}"), &b),
            (&format!("C:/outside/{nc}"), &c),
            (&nd, &d),
            (&ne, &e),
            // Content that doesn't match its name.
            (&nf, &good_crc_wrong_body),
            ("readme.txt", b"hello"),
            ("0xabc.dff", b"lower-case name"),
            ("cache/", b""),
        ]);
        let zip = root.path().join("cache.zip");
        fs::write(&zip, zip_bytes).unwrap();

        let missing = vec![
            entry(&na, a.len() as u64),
            entry(&nb, b.len() as u64),
            entry(&nc, c.len() as u64),
            entry(&nf, f.len() as u64),
        ];
        let events: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let sink = events.clone();
        let report: Reporter = Arc::new(move |p| sink.lock().unwrap().push(p));
        let unpacked = extract(&zip, &cache, &missing, &report, &Cancel::default(), None).unwrap();
        assert_eq!(
            unpacked,
            Unpacked {
                written: 3,
                bad: 0,
                blocked: 0,
            }
        );

        assert_eq!(fs::read(cache.join(&na)).unwrap(), a);
        assert_eq!(fs::read(cache.join(&nb)).unwrap(), b);
        assert_eq!(fs::read(cache.join(&nc)).unwrap(), c);
        assert_eq!(fs::read(cache.join(&nd)).unwrap(), d, "present: untouched");
        let mut names: Vec<String> = fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut expected = vec![na.clone(), nb.clone(), nc.clone(), nd.clone()];
        expected.sort();
        assert_eq!(names, expected, "nothing else, no temp files, no folders");
        assert!(!outside.exists());
        assert!(!root.path().join("readme.txt").exists());

        let events = events.lock().unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.phase, Phase::Extract);
        assert_eq!(
            last.message.as_deref(),
            Some("Extrayendo 3 de 3 archivos...")
        );
        assert_eq!(last.percent, 100.0);
        assert_eq!(
            events[0].message.as_deref(),
            Some("Extrayendo 0 de 3 archivos...")
        );
    }

    #[test]
    fn a_wrong_crc_in_the_data_is_caught_while_streaming() {
        // The central directory says the right CRC but the stored data was
        // damaged after zipping: the entry is dropped, nothing stays.
        let root = tempfile::tempdir().unwrap();
        let body = vec![7u8; 4096];
        let name = named(&body, "dff");
        let stored = {
            let mut out = io::Cursor::new(Vec::new());
            let mut zip = zip::ZipWriter::new(&mut out);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(&body).unwrap();
            zip.finish().unwrap();
            out.into_inner()
        };
        let mut damaged = stored.clone();
        let at = damaged
            .windows(64)
            .position(|w| w.iter().all(|&b| b == 7))
            .unwrap();
        damaged[at + 10] ^= 0xFF;
        let zip = root.path().join("damaged.zip");
        fs::write(&zip, damaged).unwrap();
        let missing = vec![entry(&name, body.len() as u64)];
        let unpacked = extract(
            &zip,
            root.path(),
            &missing,
            &quiet(),
            &Cancel::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            unpacked,
            Unpacked {
                written: 0,
                bad: 1,
                blocked: 0,
            }
        );
        assert!(!root.path().join(&name).exists());
        let left: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("damaged.zip")]);
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let zip = root.path().join("junk.zip");
        fs::write(&zip, vec![0x55u8; 10_000]).unwrap();
        assert!(extract(&zip, root.path(), &[], &quiet(), &Cancel::default(), None).is_err());
        assert!(extract(
            &root.path().join("missing.zip"),
            root.path(),
            &[],
            &quiet(),
            &Cancel::default(),
            None,
        )
        .is_err());
    }

    /// Incompressible, distinct bodies.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn cancel_stops_mid_way_and_leaves_no_temp() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        let total = 40;
        let bodies: Vec<Vec<u8>> = (0..total).map(|i| noise(i + 1, 256 * 1024)).collect();
        let names: Vec<String> = bodies.iter().map(|b| named(b, "dff")).collect();
        let pairs: Vec<(&str, &[u8])> = names
            .iter()
            .zip(&bodies)
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        let zip = root.path().join("cache.zip");
        fs::write(&zip, build_zip(&pairs)).unwrap();
        let missing: Vec<Entry> = names
            .iter()
            .zip(&bodies)
            .map(|(n, b)| entry(n, b.len() as u64))
            .collect();

        // Cancelled from another thread as soon as the first file lands.
        let cancel = Cancel::default();
        let unpacked = std::thread::scope(|scope| {
            let worker = scope.spawn(|| extract(&zip, &cache, &missing, &quiet(), &cancel, None));
            while !worker.is_finished() {
                let any = fs::read_dir(&cache)
                    .unwrap()
                    .flatten()
                    .any(|e| names.iter().any(|n| e.file_name() == n.as_str()));
                if any {
                    cancel.cancel();
                    break;
                }
                std::thread::yield_now();
            }
            worker.join().unwrap().unwrap()
        });
        let on_disk: Vec<String> = fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(on_disk.len(), unpacked.written);
        assert!(
            on_disk.iter().all(|n| !n.ends_with(".sarp-part")),
            "{on_disk:?}"
        );
        assert!(
            (1..total as usize).contains(&unpacked.written),
            "{unpacked:?}"
        );

        // Cancelled before it starts: nothing is written.
        let cache2 = root.path().join("cache2");
        fs::create_dir_all(&cache2).unwrap();
        let stop = Cancel::default();
        stop.cancel();
        let none = extract(&zip, &cache2, &missing, &quiet(), &stop, None).unwrap();
        assert_eq!(none, Unpacked::default());
        assert_eq!(fs::read_dir(&cache2).unwrap().count(), 0);

        // Running again picks up the rest.
        let rest: Vec<Entry> = missing
            .iter()
            .filter(|e| !on_disk.contains(&e.name))
            .cloned()
            .collect();
        let second = extract(&zip, &cache, &rest, &quiet(), &Cancel::default(), None).unwrap();
        assert_eq!(second.written + unpacked.written, total as usize);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), total as usize);
    }
}
