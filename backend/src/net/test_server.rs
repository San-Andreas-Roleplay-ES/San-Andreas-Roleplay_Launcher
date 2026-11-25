//! Tests only: a tiny HTTP/1.1 server on 127.0.0.1 that serves bodies from
//! memory (with `Range: bytes=N-`), injects faults per path and records every
//! request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub enum Fault {
    /// Always answer with this status.
    Status(u16),
    /// Answer with the status for the first `n` requests, then serve.
    FailTimes(usize, u16),
    /// Serve the body with one byte flipped (same size).
    Corrupt,
    /// Promise the full length, send half, close.
    Truncate,
    /// 302 to this location.
    Redirect(String),
    /// Send the body in pieces of `.0` bytes, `.1` ms apart.
    Trickle(usize, u64),
    /// The first `n` requests: send `.1` bytes, pause `.2` ms, then the rest.
    StallTimes(usize, usize, u64),
    /// The first `n` requests: send `.1` bytes and close the connection.
    DropTimes(usize, usize),
    /// Answer every request with the whole body (200), Range or not.
    IgnoreRange,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    /// Lower-cased names.
    pub headers: HashMap<String, String>,
}

#[derive(Default)]
struct State {
    bodies: HashMap<String, Vec<u8>>,
    faults: HashMap<String, Fault>,
    requests: Vec<Request>,
    /// Body bytes written per path.
    sent: HashMap<String, usize>,
}

#[derive(Default)]
struct Flight {
    now: AtomicUsize,
    max: AtomicUsize,
    /// `(bytes per second, piece)`: one line shared by every response.
    line: Mutex<Option<(usize, usize)>>,
    /// When the line is free for the next piece.
    next: Mutex<Option<tokio::time::Instant>>,
}

impl Flight {
    /// When this piece may go out on the shared line (now, without one).
    fn slot(&self, len: usize) -> Option<tokio::time::Instant> {
        let (rate, _) = (*self.line.lock().unwrap())?;
        let mut next = self.next.lock().unwrap();
        let now = tokio::time::Instant::now();
        let slot = next.map_or(now, |n| n.max(now));
        *next = Some(slot + Duration::from_secs_f64(len as f64 / rate as f64));
        Some(slot)
    }

    fn piece(&self) -> Option<usize> {
        self.line.lock().unwrap().map(|(_, piece)| piece)
    }
}

pub struct TestServer {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    state: Arc<Mutex<State>>,
    flight: Arc<Flight>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state: Arc<Mutex<State>> = Arc::default();
        let flight: Arc<Flight> = Arc::default();
        let (shared, counter) = (state.clone(), flight.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(serve(socket, shared.clone(), counter.clone()));
            }
        });
        Self {
            base,
            state,
            flight,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn put(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.lock().bodies.insert(path.into(), body.into());
    }

    pub fn fault(&self, path: &str, fault: Option<Fault>) {
        let mut state = self.lock();
        match fault {
            Some(fault) => state.faults.insert(path.into(), fault),
            None => state.faults.remove(path),
        };
    }

    pub fn clear_faults(&self) {
        self.lock().faults.clear();
    }

    pub fn requests(&self) -> Vec<Request> {
        self.lock().requests.clone()
    }

    pub fn hits(&self, path: &str) -> usize {
        self.lock()
            .requests
            .iter()
            .filter(|r| r.path == path)
            .count()
    }

    /// Body bytes written for `path` so far (what a client received, at most).
    pub fn sent(&self, path: &str) -> usize {
        self.lock().sent.get(path).copied().unwrap_or(0)
    }

    pub fn forget_requests(&self) {
        let mut state = self.lock();
        state.requests.clear();
        state.sent.clear();
        self.flight.max.store(0, Ordering::SeqCst);
    }

    /// Most requests served at the same time since the last `forget_requests`.
    pub fn max_in_flight(&self) -> usize {
        self.flight.max.load(Ordering::SeqCst)
    }

    /// Every body from now on shares one line of `rate` bytes/s, sent in
    /// pieces of `piece` bytes taken in turns: with N responses at once, each
    /// one gets a piece every N pieces' time (a slow connection).
    pub fn line(&self, rate: usize, piece: usize) {
        *self.flight.line.lock().unwrap() = Some((rate, piece.max(1)));
    }
}

/// `Range: bytes=N-` → N.
fn range_start(headers: &HashMap<String, String>) -> Option<usize> {
    headers
        .get("range")?
        .strip_prefix("bytes=")?
        .strip_suffix('-')?
        .parse()
        .ok()
}

/// How the body goes out.
enum Pace {
    All,
    Trickle(usize, u64),
    Stall(usize, u64),
    Drop(usize),
}

async fn serve(mut socket: TcpStream, state: Arc<Mutex<State>>, flight: Arc<Flight>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let now = flight.now.fetch_add(1, Ordering::SeqCst) + 1;
    flight.max.fetch_max(now, Ordering::SeqCst);

    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/")
        .to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let range = range_start(&headers);

    let (fault, body) = {
        let mut state = state.lock().unwrap();
        state.requests.push(Request {
            path: path.clone(),
            headers,
        });
        let fault = match state.faults.get_mut(&path) {
            Some(Fault::FailTimes(0, _) | Fault::StallTimes(0, ..) | Fault::DropTimes(0, _)) => {
                None
            }
            Some(Fault::FailTimes(n, status)) => {
                *n -= 1;
                Some(Fault::Status(*status))
            }
            Some(Fault::StallTimes(n, after, pause)) => {
                *n -= 1;
                Some(Fault::StallTimes(1, *after, *pause))
            }
            Some(Fault::DropTimes(n, after)) => {
                *n -= 1;
                Some(Fault::DropTimes(1, *after))
            }
            other => other.cloned(),
        };
        (fault, state.bodies.get(&path).cloned())
    };

    let respond = |status: u16, extra: &str, length: usize| {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {length}\r\nConnection: close\r\n{extra}\r\n"
        )
    };
    let octets = "Content-Type: application/octet-stream\r\n";
    let mut pace = Pace::All;
    let reply: (String, Vec<u8>) = match (fault, body) {
        (Some(Fault::Status(status)), _) => (respond(status, "", 0), Vec::new()),
        (Some(Fault::Redirect(to)), _) => {
            (respond(302, &format!("Location: {to}\r\n"), 0), Vec::new())
        }
        (_, None) => (respond(404, "", 0), Vec::new()),
        (Some(Fault::Corrupt), Some(mut body)) => {
            if let Some(byte) = body.last_mut() {
                *byte ^= 0xFF;
            }
            (respond(200, "", body.len()), body)
        }
        (Some(Fault::Truncate), Some(body)) => {
            let half = body[..body.len() / 2].to_vec();
            (respond(200, "", body.len()), half)
        }
        (Some(Fault::IgnoreRange), Some(body)) => (respond(200, octets, body.len()), body),
        (fault, Some(body)) => {
            pace = match fault {
                Some(Fault::Trickle(chunk, ms)) => Pace::Trickle(chunk.max(1), ms),
                Some(Fault::StallTimes(_, after, ms)) => Pace::Stall(after, ms),
                Some(Fault::DropTimes(_, after)) => Pace::Drop(after),
                _ => Pace::All,
            };
            match range {
                Some(start) if start >= body.len() => (
                    respond(
                        416,
                        &format!("Content-Range: bytes */{}\r\n", body.len()),
                        0,
                    ),
                    Vec::new(),
                ),
                Some(start) => {
                    let total = body.len();
                    let extra = format!(
                        "{octets}Content-Range: bytes {start}-{}/{total}\r\n",
                        total - 1
                    );
                    (respond(206, &extra, total - start), body[start..].to_vec())
                }
                None => (respond(200, octets, body.len()), body),
            }
        }
    };
    let _ = socket.write_all(reply.0.as_bytes()).await;
    let count = |n: usize| {
        *state.lock().unwrap().sent.entry(path.clone()).or_default() += n;
    };
    let out = Out {
        flight: &flight,
        count: &count,
    };
    let data = reply.1;
    let _ = match pace {
        Pace::All => out.send(&mut socket, &data).await,
        Pace::Trickle(chunk, ms) => {
            let mut result = Ok(());
            for (i, piece) in data.chunks(chunk).enumerate() {
                if i > 0 {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                result = out.send(&mut socket, piece).await;
                if result.is_err() {
                    break;
                }
            }
            result
        }
        Pace::Stall(after, ms) => {
            let cut = after.min(data.len());
            let first = out.send(&mut socket, &data[..cut]).await;
            tokio::time::sleep(Duration::from_millis(ms)).await;
            match first {
                Ok(()) => out.send(&mut socket, &data[cut..]).await,
                error => error,
            }
        }
        Pace::Drop(after) => out.send(&mut socket, &data[..after.min(data.len())]).await,
    };
    // Done as soon as the last byte is out: the client may already be on
    // its next request while this one closes.
    flight.now.fetch_sub(1, Ordering::SeqCst);
    let _ = socket.shutdown().await;
}

struct Out<'a, F: Fn(usize)> {
    flight: &'a Flight,
    count: &'a F,
}

impl<F: Fn(usize)> Out<'_, F> {
    async fn send(&self, socket: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
        let piece = self.flight.piece().unwrap_or(data.len()).max(1);
        for part in data.chunks(piece) {
            if let Some(slot) = self.flight.slot(part.len()) {
                tokio::time::sleep_until(slot).await;
            }
            socket.write_all(part).await?;
            socket.flush().await?;
            (self.count)(part.len());
        }
        Ok(())
    }
}
