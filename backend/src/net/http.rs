//! HTTP: the shared clients and the two download flavours (verified one-shot
//! for SA-MP files, resumable for the big zips: the GTA rip and the cache
//! package).
//!
//! Nothing caps a whole transfer: a slow line that keeps sending bytes is
//! never cut off. What times out is waiting, for the response headers or for
//! the next chunk of the body (a stall).

use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, Response};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::install::cancel::Cancel;
use crate::install::fs::sha256_file_async;
use crate::install::progress::{percent, Phase, Progress, Reporter, SpeedMeter};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(150);
const WRITE_BUFFER: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub const CANCELLED: &str = "Descarga cancelada.";
pub const SLOW: &str = "La conexión tardó demasiado.";
const STALLED: &str = "La descarga se detuvo: no llegan datos.";
const RETRYING: &str = "Se cortó la conexión. Reintentando...";

/// How long a download may wait, and how it retries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// For the response headers.
    pub response: Duration,
    /// For the next chunk of the body.
    pub stall: Duration,
    /// Before the first retry; doubles up to `max_delay`.
    pub retry_delay: Duration,
    pub max_delay: Duration,
    /// One-shot downloads: attempts in all. Resumable ones: attempts in a row
    /// that get no further than before (one that does resets the count).
    pub attempts: u32,
}

impl Limits {
    /// The big resumable downloads.
    pub const RESUMABLE: Self = Self {
        response: Duration::from_secs(30),
        stall: Duration::from_secs(60),
        retry_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        attempts: 5,
    };

    /// SA-MP files and installer.
    pub const ONE_SHOT: Self = Self {
        attempts: 3,
        stall: Duration::from_secs(30),
        ..Self::RESUMABLE
    };

    /// Wait before retry number `retry` (0-based).
    pub fn delay(&self, retry: u32) -> Duration {
        self.retry_delay
            .saturating_mul(2u32.saturating_pow(retry))
            .min(self.max_delay)
    }
}

/// Why an attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The line: no response in time, a stall, a broken connection.
    Slow(String),
    /// The server answered with a transient error (5xx, 408, 429).
    Retry(String),
    /// Not worth another attempt (4xx, redirects, disk errors, bad content).
    Fatal(String),
    Cancelled,
}

impl Failure {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Slow(_) | Self::Retry(_))
    }

    pub fn message(self) -> String {
        match self {
            Self::Slow(m) | Self::Retry(m) | Self::Fatal(m) => m,
            Self::Cancelled => CANCELLED.into(),
        }
    }

    /// For a status other than the ones the caller handles.
    pub fn status(status: u16) -> Self {
        let message = format!("HTTP {status} al descargar el archivo.");
        if status >= 500 || status == 408 || status == 429 {
            Self::Retry(message)
        } else {
            Self::Fatal(message)
        }
    }
}

pub fn client(version: &str) -> Client {
    build(version, Policy::default())
}

/// Never follows redirects: for our API (a redirect would carry
/// `X-Launcher-Key` to whatever host it names) and for the cache files and
/// package, which must come from the URLs the API gave and nowhere else.
pub fn strict_client(version: &str) -> Client {
    build(version, Policy::none())
}

fn build(version: &str, redirects: Policy) -> Client {
    Client::builder()
        .user_agent(format!("SARP-Launcher/{version}"))
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirects)
        .build()
        .unwrap_or_else(|error| {
            log::error!("custom HTTP client failed to build, using defaults: {error}");
            Client::new()
        })
}

/// Readable message for a transport error. The full chain goes to the log.
pub fn net_error(error: &reqwest::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        detail.push_str(&format!(": {inner}"));
        source = inner.source();
    }
    log::warn!("network error: {detail}");
    if error.is_timeout() {
        SLOW.into()
    } else if error.is_connect() {
        "No se pudo conectar con el servidor. Revisa tu conexión a Internet.".into()
    } else if error.is_body() || error.is_decode() {
        "Se interrumpió la descarga.".into()
    } else {
        format!("Error de red: {error}")
    }
}

/// Sends `request`, waiting at most `wait` for the response headers.
pub async fn send(
    request: RequestBuilder,
    wait: Duration,
    cancel: &Cancel,
) -> Result<Response, Failure> {
    match cancel.run(tokio::time::timeout(wait, request.send())).await {
        None => Err(Failure::Cancelled),
        Some(Err(_)) => Err(Failure::Slow(SLOW.into())),
        Some(Ok(Err(error))) => Err(Failure::Slow(net_error(&error))),
        Some(Ok(Ok(response))) => Ok(response),
    }
}

/// The next chunk of a body (`None` at the end), waiting at most `stall`.
pub async fn next_chunk<S, B>(
    body: &mut S,
    stall: Duration,
    cancel: &Cancel,
) -> Result<Option<B>, Failure>
where
    S: Stream<Item = reqwest::Result<B>> + Unpin,
{
    match cancel.run(tokio::time::timeout(stall, body.next())).await {
        None => Err(Failure::Cancelled),
        Some(Err(_)) => Err(Failure::Slow(STALLED.into())),
        Some(Ok(None)) => Ok(None),
        Some(Ok(Some(Err(error)))) => Err(Failure::Slow(net_error(&error))),
        Some(Ok(Some(Ok(chunk)))) => Ok(Some(chunk)),
    }
}

/// A whole (small) body. A slow one is fine as long as it keeps arriving.
pub async fn read_body(response: Response, stall: Duration) -> Result<Vec<u8>, String> {
    let never = Cancel::default();
    let mut body = response.bytes_stream();
    let mut out = Vec::new();
    while let Some(chunk) = next_chunk(&mut body, stall, &never)
        .await
        .map_err(Failure::message)?
    {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Downloads `url` to `dest`, checking size and SHA-256, with a few retries
/// (each one starts over). `on_chunk` receives the running byte count.
pub async fn download_verified(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
    expected_sha256: &str,
    mut on_chunk: impl FnMut(u64),
) -> Result<(), String> {
    let limits = Limits::ONE_SHOT;
    let mut retry = 0;
    loop {
        let attempt = verified_once(
            client,
            url,
            dest,
            expected_size,
            expected_sha256,
            &limits,
            &mut on_chunk,
        );
        match attempt.await {
            Ok(()) => return Ok(()),
            Err(failure) if failure.retryable() && retry + 1 < limits.attempts => {
                let delay = limits.delay(retry);
                log::info!("download {url} failed ({failure:?}), retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                retry += 1;
            }
            Err(failure) => return Err(failure.message()),
        }
    }
}

async fn verified_once(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
    expected_sha256: &str,
    limits: &Limits,
    on_chunk: &mut impl FnMut(u64),
) -> Result<(), Failure> {
    let never = Cancel::default();
    let response = send(client.get(url), limits.response, &never).await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let message = format!("HTTP {status} al descargar {}.", dest.display());
        return Err(if Failure::status(status).retryable() {
            Failure::Retry(message)
        } else {
            Failure::Fatal(message)
        });
    }
    let fatal = |e: std::io::Error| Failure::Fatal(e.to_string());
    let file = tokio::fs::File::create(dest).await.map_err(fatal)?;
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut body = response.bytes_stream();
    while let Some(chunk) = next_chunk(&mut body, limits.stall, &never).await? {
        received += chunk.len() as u64;
        hasher.update(&chunk);
        writer.write_all(&chunk).await.map_err(fatal)?;
        on_chunk(received);
    }
    writer.flush().await.map_err(fatal)?;
    if received != expected_size {
        return Err(Failure::Slow(
            "El archivo descargado está incompleto.".into(),
        ));
    }
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected_sha256) {
        return Err(Failure::Fatal(
            "El archivo descargado no coincide con el original.".into(),
        ));
    }
    Ok(())
}

/// What a file already at `dest` may be used for.
#[derive(Debug, Clone, Copy)]
pub enum Leftover<'a> {
    /// A shorter one is resumed; a complete one is reused only if its SHA-256
    /// matches.
    Sha256(&'a str),
    /// `dest` is named after this exact upload (URL and size): a shorter file
    /// is resumed and a complete one reused as is. The caller checks the
    /// content.
    Trusted,
}

pub struct ResumeOptions<'a> {
    pub url: &'a str,
    pub dest: &'a Path,
    pub expected_size: u64,
    pub leftover: Leftover<'a>,
    pub limits: Limits,
    /// Progress title (`None`: the UI's own).
    pub message: Option<&'a str>,
}

impl ResumeOptions<'_> {
    fn progress(&self, done: u64) -> Progress {
        let progress = Progress::new(Phase::Download, percent(done, self.expected_size))
            .bytes(done, self.expected_size);
        match self.message {
            Some(message) => progress.message(message),
            None => progress,
        }
    }
}

enum Attempt {
    Complete,
    /// The partial file can't be continued (416, a size that doesn't match):
    /// wipe it and start over, once.
    Restart(String),
}

/// Resumable download with throughput-aware progress. Keeps retrying (with
/// backoff) as long as attempts get further into the file; gives up after
/// `limits.attempts` in a row that don't. Whatever reached the disk stays
/// for the next call, also after a cancel or a failure.
pub async fn download_with_resume(
    client: &Client,
    opts: &ResumeOptions<'_>,
    report: &Reporter,
    cancel: &Cancel,
) -> Result<(), String> {
    let mut restarted = false;
    let mut best = 0u64;
    let mut idle = 0u32;
    let mut retry = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(CANCELLED.into());
        }
        let Some(offset) = prepare_existing(opts, report).await? else {
            return Ok(());
        };
        best = best.max(offset);
        let failure = match transfer(client, opts, offset, report, cancel).await {
            Ok(Attempt::Complete) => return Ok(()),
            Ok(Attempt::Restart(message)) => {
                remove(opts.dest).await?;
                if restarted {
                    return Err(message);
                }
                log::warn!("download {}: {message}; starting over", opts.url);
                restarted = true;
                continue;
            }
            Err(failure) => failure,
        };
        if !failure.retryable() {
            return Err(failure.message());
        }
        // Progress is the furthest point reached: a server that ignores
        // Range restarts from zero and only counts once it gets further.
        let reached = file_len(opts.dest).await;
        if reached > best {
            best = reached;
            idle = 0;
            retry = 0;
        } else {
            idle += 1;
        }
        let message = failure.message();
        if idle >= opts.limits.attempts {
            log::warn!("download {}: giving up: {message}", opts.url);
            return Err(message);
        }
        let delay = opts.limits.delay(retry);
        retry += 1;
        log::info!(
            "download {}: {message}; retrying in {delay:?} from byte {reached}",
            opts.url
        );
        report(
            Progress::new(Phase::Download, percent(reached, opts.expected_size))
                .bytes(reached, opts.expected_size)
                .message(RETRYING),
        );
        if !cancel.sleep(delay).await {
            return Err(CANCELLED.into());
        }
    }
}

async fn file_len(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

async fn remove(path: &Path) -> Result<(), String> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
        _ => Ok(()),
    }
}

/// Decides what to do with a file already at `dest`. `None` = already
/// complete (and verified, if there is a hash); `Some(offset)` = download
/// from `offset`.
async fn prepare_existing(
    opts: &ResumeOptions<'_>,
    report: &Reporter,
) -> Result<Option<u64>, String> {
    let Ok(meta) = tokio::fs::metadata(opts.dest).await else {
        return Ok(Some(0));
    };
    let existing = meta.len();
    if existing < opts.expected_size {
        return Ok(Some(existing));
    }
    let complete = existing == opts.expected_size
        && match opts.leftover {
            Leftover::Trusted => true,
            Leftover::Sha256(sha) => sha256_file_async(opts.dest.to_path_buf())
                .await
                .is_ok_and(|actual| actual.eq_ignore_ascii_case(sha)),
        };
    if complete {
        report(opts.progress(opts.expected_size));
        return Ok(None);
    }
    // Larger than expected, or the hash is wrong: start over.
    remove(opts.dest).await?;
    Ok(Some(0))
}

/// `bytes <start>-<end>/<total>` → `(start, total)` (`None` total for `*`).
fn content_range(response: &Response) -> Option<(u64, Option<u64>)> {
    let raw = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (range, total) = raw.strip_prefix("bytes ")?.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

async fn transfer(
    client: &Client,
    opts: &ResumeOptions<'_>,
    offset: u64,
    report: &Reporter,
    cancel: &Cancel,
) -> Result<Attempt, Failure> {
    let size = opts.expected_size;
    let mut request = client.get(opts.url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    // The resumed point shows right away, before the first byte arrives.
    report(opts.progress(offset));
    let response = send(request, opts.limits.response, cancel).await?;
    let start = match response.status().as_u16() {
        // Range ignored (or not asked for): the body is the whole file.
        200 => 0,
        206 => match content_range(&response) {
            Some((start, total)) if start == offset && total.is_none_or(|t| t == size) => start,
            other => {
                return Ok(Attempt::Restart(format!(
                    "Content-Range {other:?} for offset {offset} of {size}"
                )))
            }
        },
        416 => return Ok(Attempt::Restart("HTTP 416 al descargar el archivo.".into())),
        status => return Err(Failure::status(status)),
    };
    if let Some(length) = response.content_length().filter(|&l| l != size - start) {
        return Ok(Attempt::Restart(format!(
            "El archivo del servidor no tiene el tamaño esperado ({length} de {size} bytes)."
        )));
    }
    if start == 0 && offset > 0 {
        log::info!("{} ignored the Range request; starting over", opts.url);
    }

    let fatal = |e: std::io::Error| Failure::Fatal(e.to_string());
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(start > 0)
        .truncate(start == 0)
        .open(opts.dest)
        .await
        .map_err(fatal)?;
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut received = start;
    let mut overflow = false;
    let mut speed = SpeedMeter::default();
    let mut last_emit = Some(Instant::now());
    let mut body = response.bytes_stream();
    let written: Result<(), Failure> = async {
        while let Some(chunk) = next_chunk(&mut body, opts.limits.stall, cancel).await? {
            if received + chunk.len() as u64 > size {
                overflow = true;
                return Ok(());
            }
            writer.write_all(&chunk).await.map_err(fatal)?;
            received += chunk.len() as u64;
            let now = Instant::now();
            speed.push(now, chunk.len() as u64);
            // A fast line produces dozens of chunks per second.
            if last_emit.is_some_and(|t| now.duration_since(t) < PROGRESS_THROTTLE) {
                continue;
            }
            last_emit = Some(now);
            report(Progress {
                speed_bps: Some(speed.bytes_per_second(now)),
                ..opts.progress(received)
            });
        }
        Ok(())
    }
    .await;
    // Whatever arrived stays on disk for the next attempt.
    let flushed = writer.flush().await;
    let file = writer.into_inner().into_std().await;
    // Closing a big new file is where real-time antivirus scans it.
    let _ = tokio::task::spawn_blocking(move || drop(file)).await;
    written?;
    if overflow {
        return Ok(Attempt::Restart(format!(
            "El archivo del servidor tiene más de los {size} bytes esperados."
        )));
    }
    flushed.map_err(fatal)?;
    if received != size {
        return Err(Failure::Slow("La descarga quedó incompleta.".into()));
    }
    report(opts.progress(size));
    Ok(Attempt::Complete)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::test_server::{Fault, TestServer};
    use std::sync::{Arc, Mutex};

    pub fn fast() -> Limits {
        Limits {
            response: Duration::from_secs(5),
            stall: Duration::from_millis(400),
            retry_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
            attempts: 3,
        }
    }

    fn collector() -> (Reporter, Arc<Mutex<Vec<Progress>>>) {
        let events: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let sink = events.clone();
        (Arc::new(move |p| sink.lock().unwrap().push(p)), events)
    }

    fn test_client() -> Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        client("test")
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn opts<'a>(url: &'a str, dest: &'a Path, size: usize, limits: Limits) -> ResumeOptions<'a> {
        ResumeOptions {
            url,
            dest,
            expected_size: size as u64,
            leftover: Leftover::Trusted,
            limits,
            message: Some("Descargando paquete del caché..."),
        }
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let limits = Limits::RESUMABLE;
        assert_eq!(limits.delay(0), Duration::from_secs(1));
        assert_eq!(limits.delay(1), Duration::from_secs(2));
        assert_eq!(limits.delay(3), Duration::from_secs(8));
        assert_eq!(limits.delay(10), Duration::from_secs(30));
        assert_eq!(limits.delay(u32::MAX), Duration::from_secs(30));
        // No total cap anywhere: only waits.
        assert_eq!(Limits::ONE_SHOT.stall, Duration::from_secs(30));
        assert_eq!(Limits::RESUMABLE.stall, Duration::from_secs(60));
    }

    #[test]
    fn failures_classify_statuses() {
        assert!(Failure::status(503).retryable());
        assert!(Failure::status(429).retryable());
        assert!(Failure::status(408).retryable());
        assert!(!Failure::status(404).retryable());
        assert!(!Failure::status(302).retryable());
        assert!(!Failure::status(403).retryable());
        assert!(!Failure::Cancelled.retryable());
        assert_eq!(Failure::Cancelled.message(), CANCELLED);
    }

    #[tokio::test]
    async fn a_slow_steady_transfer_is_never_cut_off() {
        // 128 KB in 2 KB pieces every 40 ms: ~2.5 s in all, well over the
        // 1 s stall timeout (each gap is a 25th of it). Only a gap longer
        // than the stall timeout fails.
        let server = TestServer::start().await;
        let data = body(128 * 1024);
        server.put("/slow.zip", data.clone());
        server.fault("/slow.zip", Some(Fault::Trickle(2048, 40)));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("slow.zip");
        let url = format!("{}/slow.zip", server.base);
        let (report, events) = collector();
        let started = Instant::now();
        let limits = Limits {
            stall: Duration::from_secs(1),
            ..fast()
        };
        download_with_resume(
            &test_client(),
            &opts(&url, &dest, data.len(), limits),
            &report,
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() > limits.stall * 2,
            "{:?}",
            started.elapsed()
        );
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(server.hits("/slow.zip"), 1);
        {
            let events = events.lock().unwrap();
            assert!(events.iter().any(|p| p.speed_bps.is_some()));
            assert_eq!(events.last().unwrap().bytes_done, Some(data.len() as u64));
        }

        // The same body with a gap longer than the stall timeout fails the
        // attempt; the retry resumes and finishes it.
        server.fault("/slow.zip", Some(Fault::StallTimes(1, 20 * 1024, 3000)));
        std::fs::remove_file(&dest).unwrap();
        server.forget_requests();
        download_with_resume(
            &test_client(),
            &opts(&url, &dest, data.len(), limits),
            &report,
            &Cancel::default(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].headers.get("range").map(String::as_str),
            Some("bytes=20480-")
        );
    }

    #[tokio::test]
    async fn one_shot_downloads_have_no_total_cap_either() {
        let server = TestServer::start().await;
        // 96 KB in 4 KB pieces every 60 ms: ~1.4 s, over the 1 s stall
        // timeout (each gap a 16th of it).
        let data = body(96 * 1024);
        let sha = hex::encode(Sha256::digest(&data));
        server.put("/samp.exe", data.clone());
        server.fault("/samp.exe", Some(Fault::Trickle(4096, 60)));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("samp.exe");
        let mut last = 0;
        let started = Instant::now();
        let limits = Limits {
            stall: Duration::from_secs(1),
            ..fast()
        };
        verified_once(
            &test_client(),
            &format!("{}/samp.exe", server.base),
            &dest,
            data.len() as u64,
            &sha,
            &limits,
            &mut |n| last = n,
        )
        .await
        .unwrap();
        assert!(started.elapsed() > limits.stall);
        assert_eq!(last, data.len() as u64);

        // A stall longer than the limit is a retryable failure.
        server.fault("/samp.exe", Some(Fault::StallTimes(1, 4096, 3000)));
        let error = verified_once(
            &test_client(),
            &format!("{}/samp.exe", server.base),
            &dest,
            data.len() as u64,
            &sha,
            &limits,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error, Failure::Slow(STALLED.into()));
        assert!(error.retryable());
    }

    // ---- Network tests (run with `cargo test -- --ignored`) ----

    const SAMP_EXE_URL: &str = "https://sarp-public.b-cdn.net/samp/samp.exe";
    const SAMP_EXE_SIZE: u64 = 413_696;
    const SAMP_EXE_SHA: &str = "b21351fbcd10ffa0f47a453bc2d193cc820919dba7240cdda7a32c21bb21fb14";

    #[tokio::test]
    #[ignore = "network"]
    async fn downloads_samp_exe_verified() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("samp.exe");
        let mut last = 0;
        download_verified(
            &test_client(),
            SAMP_EXE_URL,
            &dest,
            SAMP_EXE_SIZE,
            SAMP_EXE_SHA,
            |n| last = n,
        )
        .await
        .unwrap();
        assert_eq!(last, SAMP_EXE_SIZE);
        assert_eq!(
            crate::install::fs::sha256_file(&dest).unwrap(),
            SAMP_EXE_SHA
        );

        // A wrong hash must be rejected.
        let err = download_verified(
            &test_client(),
            SAMP_EXE_URL,
            &dest,
            SAMP_EXE_SIZE,
            &"0".repeat(64),
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err, "El archivo descargado no coincide con el original.");
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn resumes_a_partial_download_and_verifies() {
        let client = test_client();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("samp.exe");

        // Get the real bytes, then truncate to fake an interrupted download.
        download_verified(
            &client,
            SAMP_EXE_URL,
            &dest,
            SAMP_EXE_SIZE,
            SAMP_EXE_SHA,
            |_| {},
        )
        .await
        .unwrap();
        let full = std::fs::read(&dest).unwrap();
        let partial = 100_000usize;
        std::fs::write(&dest, &full[..partial]).unwrap();

        let (report, events) = collector();
        let opts = ResumeOptions {
            url: SAMP_EXE_URL,
            dest: &dest,
            expected_size: SAMP_EXE_SIZE,
            leftover: Leftover::Sha256(SAMP_EXE_SHA),
            limits: Limits::RESUMABLE,
            message: None,
        };
        let never = Cancel::default();
        download_with_resume(&client, &opts, &report, &never)
            .await
            .unwrap();
        assert_eq!(
            crate::install::fs::sha256_file(&dest).unwrap(),
            SAMP_EXE_SHA
        );

        {
            let mut seen = events.lock().unwrap();
            let first = seen.first().expect("progress events");
            // The first report already counts the bytes we resumed from.
            assert_eq!(first.bytes_done, Some(partial as u64), "{first:?}");
            assert!(seen.iter().all(|p| p.phase == Phase::Download));
            seen.clear();
        }

        // Complete + matching hash on disk: reused without a request — a
        // single 100% report and no speed sample.
        download_with_resume(&client, &opts, &report, &never)
            .await
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![Progress::new(Phase::Download, 100.0).bytes(SAMP_EXE_SIZE, SAMP_EXE_SIZE)]
        );

        // Corrupt (larger than expected) file: discarded and re-fetched.
        std::fs::write(&dest, vec![0u8; SAMP_EXE_SIZE as usize + 10]).unwrap();
        download_with_resume(&client, &opts, &report, &never)
            .await
            .unwrap();
        assert_eq!(
            crate::install::fs::sha256_file(&dest).unwrap(),
            SAMP_EXE_SHA
        );
    }
}
