//! Zip extraction tuned for slow drives (the GTA rip).
//!
//! - Two passes: plan (validate every path, sum sizes, collect folders), then
//!   extract serially — parallel writes thrash HDD heads.
//! - Every folder is created once up front.
//! - 256 KB write buffer to amortise syscalls.
//! - Progress counts uncompressed bytes, so the bar tracks time even when a
//!   single entry (gta3.img) is hundreds of MB.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zip::ZipArchive;

use crate::install::progress::{percent, Phase, Progress, Reporter};

const WRITE_BUFFER: usize = 256 * 1024;
const READ_CHUNK: usize = 64 * 1024;
const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);

/// Turns a zip entry name into a relative path that cannot escape the target
/// folder. `None` when the name is absolute (`/x`, `\x`, `C:...`), climbs out
/// with `..`, or contains characters that would address something other than
/// a plain file (`:` → NTFS alternate data streams, NUL).
pub fn safe_relative_path(name: &str) -> Option<PathBuf> {
    if name.contains('\0') {
        return None;
    }
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') {
        return None;
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part if part.contains(':') => return None,
            part => parts.push(part),
        }
    }
    Some(parts.iter().collect())
}

struct Plan {
    dirs: BTreeSet<PathBuf>,
    files: Vec<(usize, PathBuf, u64)>,
    total_bytes: u64,
}

fn open(zip_path: &Path) -> Result<ZipArchive<BufReader<File>>, String> {
    let file = File::open(zip_path).map_err(|e| {
        log::warn!("open {}: {e}", zip_path.display());
        "No se pudo abrir el zip.".to_string()
    })?;
    ZipArchive::new(BufReader::new(file)).map_err(|e| {
        log::warn!("read zip {}: {e}", zip_path.display());
        "No se pudo abrir el zip.".to_string()
    })
}

fn plan<R: Read + std::io::Seek>(archive: &ZipArchive<R>) -> Result<Plan, String> {
    let mut plan = Plan {
        dirs: BTreeSet::new(),
        files: Vec::new(),
        total_bytes: 0,
    };
    for index in 0..archive.len() {
        let entry = archive
            .by_index_data(index)
            .map_err(|e| format!("No se pudo leer el zip: {e}"))?;
        let name = entry
            .name()
            .map_err(|e| format!("No se pudo leer el zip: {e}"))?
            .into_owned();
        let rel = safe_relative_path(&name)
            .ok_or_else(|| format!("Ruta no permitida en el zip: {name}"))?;
        if entry.is_dir() {
            if rel.as_os_str().is_empty() {
                continue;
            }
            plan.dirs.insert(rel);
        } else {
            if rel.as_os_str().is_empty() {
                return Err(format!("Ruta no permitida en el zip: {name}"));
            }
            if let Some(parent) = rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                plan.dirs.insert(parent.to_path_buf());
            }
            plan.total_bytes += entry.size();
            plan.files.push((index, rel, entry.size()));
        }
    }
    Ok(plan)
}

/// Extracts `zip_path` into `target_dir`. Blocking: run on a blocking thread.
pub fn extract(zip_path: &Path, target_dir: &Path, report: &Reporter) -> Result<(), String> {
    let mut archive = open(zip_path)?;
    let plan = plan(&archive)?;

    // BTreeSet order puts parents before children; create_dir_all is cheap
    // for the ones that already exist.
    for dir in &plan.dirs {
        fs::create_dir_all(target_dir.join(dir)).map_err(|e| e.to_string())?;
    }

    let total_entries = plan.files.len();
    let mut processed_bytes = 0u64;
    let mut processed_entries = 0usize;
    let mut last_emit: Option<Instant> = None;
    let mut emit = |bytes: u64, entries: usize, force: bool| {
        let now = Instant::now();
        if !force && last_emit.is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE) {
            return;
        }
        last_emit = Some(now);
        report(
            Progress::new(Phase::Extract, percent(bytes, plan.total_bytes))
                .bytes(bytes, plan.total_bytes)
                .message(format!(
                    "Extrayendo {entries} de {total_entries} archivos..."
                )),
        );
    };

    emit(0, 0, true);
    let result = (|| {
        let mut buf = vec![0u8; READ_CHUNK];
        for (index, rel, size) in &plan.files {
            let mut entry = archive.by_index(*index).map_err(|e| {
                log::warn!("zip entry {}: {e}", rel.display());
                format!("No se pudo abrir {}", rel.display())
            })?;
            let file = File::create(target_dir.join(rel)).map_err(|e| e.to_string())?;
            let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
            let mut written = 0u64;
            loop {
                let n = entry.read(&mut buf).map_err(|e| {
                    log::warn!("inflate {}: {e}", rel.display());
                    format!("El zip está dañado ({}).", rel.display())
                })?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                written += n as u64;
                processed_bytes += n as u64;
                emit(processed_bytes, processed_entries, false);
            }
            writer.flush().map_err(|e| e.to_string())?;
            // Never let the counter drift behind what the plan promised.
            if *size > written {
                processed_bytes += size - written;
            }
            processed_entries += 1;
            emit(processed_bytes, processed_entries, false);
        }
        Ok(())
    })();
    emit(processed_bytes, processed_entries, true);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use zip::write::SimpleFileOptions;

    fn build_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            match data {
                Some(data) => {
                    zip.start_file(*name, options).unwrap();
                    zip.write_all(data).unwrap();
                }
                None => zip.add_directory(*name, options).unwrap(),
            }
        }
        zip.finish().unwrap();
    }

    fn collector() -> (Reporter, Arc<Mutex<Vec<Progress>>>) {
        let events: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let sink = events.clone();
        (Arc::new(move |p| sink.lock().unwrap().push(p)), events)
    }

    #[test]
    fn safe_paths() {
        let ok = |s: &str| safe_relative_path(s).map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(ok("models/gta3.img").as_deref(), Some("models/gta3.img"));
        assert_eq!(ok("a/./b/../c.txt").as_deref(), Some("a/c.txt"));
        assert_eq!(ok("dir\\file.txt").as_deref(), Some("dir/file.txt"));
        assert_eq!(ok("folder/").as_deref(), Some("folder"));
        assert_eq!(ok("./").as_deref(), Some(""));

        for bad in [
            "../evil.dll",
            "a/../../evil.dll",
            "..\\evil.dll",
            "/etc/passwd",
            "\\Windows\\evil.dll",
            "C:/Windows/evil.dll",
            "c:evil.dll",
            "file.txt:Zone.Identifier",
            "dir/a:b",
            "nul\0byte",
        ] {
            assert_eq!(safe_relative_path(bad), None, "{bad} should be rejected");
        }
    }

    #[test]
    fn extracts_files_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("test.zip");
        let big = vec![7u8; 300_000];
        build_zip(
            &zip_path,
            &[
                ("empty-dir/", None),
                ("gta_sa.exe", Some(b"exe")),
                ("models/gta3.img", Some(&big)),
                ("data/sub/deep.txt", Some(b"deep")),
            ],
        );
        let target = dir.path().join("out");
        fs::create_dir(&target).unwrap();
        let (report, events) = collector();
        extract(&zip_path, &target, &report).unwrap();

        assert_eq!(fs::read(target.join("gta_sa.exe")).unwrap(), b"exe");
        assert_eq!(fs::read(target.join("models/gta3.img")).unwrap(), big);
        assert_eq!(fs::read(target.join("data/sub/deep.txt")).unwrap(), b"deep");
        assert!(target.join("empty-dir").is_dir());

        let events = events.lock().unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.phase, Phase::Extract);
        assert_eq!(last.percent, 100.0);
        assert_eq!(last.bytes_done, Some(300_007));
        assert_eq!(last.bytes_total, Some(300_007));
        assert_eq!(
            last.message.as_deref(),
            Some("Extrayendo 3 de 3 archivos...")
        );
        assert_eq!(events.first().unwrap().bytes_done, Some(0));
    }

    #[test]
    fn rejects_path_traversal_before_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("evil.zip");
        build_zip(
            &zip_path,
            &[
                ("good.txt", Some(b"ok")),
                ("../escaped.txt", Some(b"pwned")),
            ],
        );
        let target = dir.path().join("out");
        fs::create_dir(&target).unwrap();
        let (report, _) = collector();
        let err = extract(&zip_path, &target, &report).unwrap_err();
        assert_eq!(err, "Ruta no permitida en el zip: ../escaped.txt");
        assert!(!dir.path().join("escaped.txt").exists());
        // The plan pass fails first, so not even the good entry was written.
        assert!(!target.join("good.txt").exists());
    }

    #[test]
    fn rejects_a_non_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("fake.zip");
        fs::write(&zip_path, b"definitely not a zip").unwrap();
        let (report, _) = collector();
        assert_eq!(
            extract(&zip_path, dir.path(), &report).unwrap_err(),
            "No se pudo abrir el zip."
        );
    }
}
