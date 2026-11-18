//! A web server for one file that can be told to misbehave.
//!
//! It speaks just enough HTTP/1.1 to answer a `GET`, and is written out by
//! hand so that a test can make it do what no real server library will:
//! stop in the middle of a file, answer with the wrong part of it, or
//! start serving another version halfway through. A connection carries one
//! request after another, as long as each answer said how long it was and
//! was sent whole.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the server sends, and how it goes wrong.
#[derive(Clone, Default)]
pub(crate) struct Behaviour {
    /// The bytes of the file.
    pub file: Vec<u8>,
    /// Whether a part of the file is sent when one is asked for.
    pub ranges: bool,
    /// The entity tag sent with the file, if any.
    pub etag: Option<String>,
    /// The time of last change sent with the file, if any.
    pub last_modified: Option<String>,
    /// Sent as `Repr-Digest`, if set.
    pub repr_digest: Option<String>,
    /// Answer every request with this status and no file.
    pub status: Option<u16>,
    /// Do not say how long the body is.
    pub no_length: bool,
    /// The one thing the server gets wrong in what it answers, if any.
    pub mistake: Option<Mistake>,
    /// The most connections it serves at once, if it has a limit. A
    /// request on one more is refused.
    pub crowd: Option<usize>,
}

/// A way of answering that no server should have.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mistake {
    /// A request for a part is answered with the part a byte before it.
    PartBefore,
    /// A part is sent whatever version of the file it was asked of.
    AnyVersion,
    /// A request for the whole file is answered as one for a part.
    PartUnasked,
}

/// What goes wrong with an answer's body, for the next few answers that
/// carry one.
#[derive(Clone, Copy)]
pub(crate) enum Fault {
    /// Close the connection after this many bytes of the body.
    Cut(usize),
    /// Send this many bytes of the body and then nothing, for a long time.
    Stall(usize),
}

pub(crate) struct Server {
    pub addr: SocketAddr,
    state: Arc<State>,
}

struct State {
    behaviour: Mutex<Behaviour>,
    /// The fault for the next answers that carry a body, and how many.
    fault: Mutex<Option<(Fault, usize)>>,
    /// What each request asked for: the `Range` header, or nothing.
    asked: Mutex<Vec<Option<String>>>,
    /// How many connections were made.
    connections: AtomicUsize,
    /// How many bytes of the file were sent, all told.
    sent: AtomicUsize,
    /// How many connections are open, and how many requests were refused
    /// for coming on one too many.
    open: AtomicUsize,
    refused: AtomicUsize,
}

impl Server {
    pub(crate) async fn start(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State {
            behaviour: Mutex::new(behaviour),
            fault: Mutex::new(None),
            asked: Mutex::default(),
            connections: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            open: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
        });
        let serving = state.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let state = serving.clone();
                state.connections.fetch_add(1, Ordering::Relaxed);
                state.open.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(async move {
                    while answer(&mut stream, &state).await {}
                    state.open.fetch_sub(1, Ordering::Relaxed);
                });
            }
        });
        Self { addr, state }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Changes what the server sends from now on.
    pub(crate) fn change(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.state.behaviour.lock().unwrap());
    }

    /// Makes the next `times` answers that carry a body go wrong.
    pub(crate) fn fail(&self, fault: Fault, times: usize) {
        *self.state.fault.lock().unwrap() = (times > 0).then_some((fault, times));
    }

    /// Stops making answers go wrong.
    pub(crate) fn heal(&self) {
        *self.state.fault.lock().unwrap() = None;
    }

    /// The `Range` header of every request so far.
    pub(crate) fn asked(&self) -> Vec<Option<String>> {
        self.state.asked.lock().unwrap().clone()
    }

    /// How many connections have been made so far.
    pub(crate) fn connections(&self) -> usize {
        self.state.connections.load(Ordering::Relaxed)
    }

    /// How many bytes of the file have been sent so far, all told.
    pub(crate) fn sent(&self) -> usize {
        self.state.sent.load(Ordering::Relaxed)
    }

    /// How many requests were refused for coming on one connection too
    /// many.
    pub(crate) fn refused(&self) -> usize {
        self.state.refused.load(Ordering::Relaxed)
    }
}

/// Answers one request. Returns whether the connection can carry another.
async fn answer(stream: &mut TcpStream, state: &State) -> bool {
    let Some(request) = read_head(stream).await else {
        return false;
    };
    let header = |name: &str| {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let range = header("range");
    state.asked.lock().unwrap().push(range.clone());
    let behaviour = state.behaviour.lock().unwrap().clone();

    if let Some(head) = instead_of_the_file(&path, &behaviour, state) {
        let _ = stream.write_all(head.as_bytes()).await;
        return false;
    }

    let size = behaviour.file.len();
    // A part is sent only if the version asked for is the one there is.
    let condition = header("if-range");
    let mistake = |made| behaviour.mistake == Some(made);
    let same = mistake(Mistake::AnyVersion)
        || condition.as_ref().is_none_or(|wanted| {
            Some(wanted) == behaviour.etag.as_ref()
                || Some(wanted) == behaviour.last_modified.as_ref()
        });
    let whole = (mistake(Mistake::PartUnasked) && size > 0).then(|| Asked::Part(0, size - 1));
    let wanted = range
        .as_deref()
        .filter(|_| behaviour.ranges && same)
        .and_then(|range| parse_range(range, size))
        .or(whole);

    let mut head = String::new();
    let body = match wanted {
        Some(Asked::Outside) => {
            let _ = write!(
                head,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\n"
            );
            &[][..]
        }
        Some(Asked::Part(mut first, last)) => {
            if mistake(Mistake::PartBefore) && first > 0 {
                first -= 1;
            }
            let _ = write!(
                head,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {first}-{last}/{size}\r\n"
            );
            &behaviour.file[first..=last]
        }
        None => {
            head.push_str("HTTP/1.1 200 OK\r\n");
            &behaviour.file[..]
        }
    };
    if !behaviour.no_length {
        let _ = write!(head, "Content-Length: {}\r\n", body.len());
    }
    for (name, value) in [
        ("ETag", &behaviour.etag),
        ("Last-Modified", &behaviour.last_modified),
        ("Repr-Digest", &behaviour.repr_digest),
    ] {
        if let Some(value) = value {
            let _ = write!(head, "{name}: {value}\r\n");
        }
    }
    if behaviour.ranges {
        head.push_str("Accept-Ranges: bytes\r\n");
    }
    // An answer that does not say how long it is ends where the connection
    // does.
    if behaviour.no_length {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).await.is_err() {
        return false;
    }

    let whole = send(stream, body, state).await;
    if !whole || behaviour.no_length {
        let _ = stream.shutdown().await;
        return false;
    }
    true
}

/// The answer to a request that is not answered with the file, if this is
/// one: the asker is sent on to another address, or the server has too
/// many connections open, or it is set to refuse everything.
fn instead_of_the_file(path: &str, behaviour: &Behaviour, state: &State) -> Option<String> {
    let closing = "Content-Length: 0\r\nConnection: close\r\n\r\n";
    // One address sends the asker on to the file, and one back to itself.
    let onward = match path {
        "/moved" => Some("/sample.bin"),
        "/loop" => Some("/loop"),
        _ => None,
    };
    if let Some(onward) = onward {
        return Some(format!(
            "HTTP/1.1 302 Found\r\nLocation: {onward}\r\n{closing}"
        ));
    }
    let open = state.open.load(Ordering::Relaxed);
    if behaviour.crowd.is_some_and(|most| open > most) {
        state.refused.fetch_add(1, Ordering::Relaxed);
        return Some(format!("HTTP/1.1 503 Busy\r\n{closing}"));
    }
    let status = behaviour.status?;
    Some(format!("HTTP/1.1 {status} Refused\r\n{closing}"))
}

/// Sends the body of an answer, as far as the fault that is due lets it.
/// Returns whether all of it was sent.
async fn send(stream: &mut TcpStream, body: &[u8], state: &State) -> bool {
    // A fault is spent only on an answer that has more body than a byte:
    // the first request of a transfer asks for one byte to learn the size.
    let fault = (body.len() > 1)
        .then(|| {
            let mut pending = state.fault.lock().unwrap();
            let (fault, left) = (*pending)?;
            *pending = (left > 1).then_some((fault, left - 1));
            Some(fault)
        })
        .flatten();
    let sending = match fault {
        Some(Fault::Cut(after) | Fault::Stall(after)) => &body[..after.min(body.len())],
        None => body,
    };
    // In pieces, so that a reader that stops is noticed and not sent to.
    for piece in sending.chunks(16 * 1024) {
        if stream.write_all(piece).await.is_err() {
            return false;
        }
        state.sent.fetch_add(piece.len(), Ordering::Relaxed);
    }
    if let Some(Fault::Stall(_)) = fault {
        let _ = stream.flush().await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
    fault.is_none()
}

/// Reads a request as far as the empty line that ends its headers.
async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
        if stream.read_exact(&mut byte).await.is_err() {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

/// What a `Range` header asks for.
enum Asked {
    /// The bytes from the first of these to the second.
    Part(usize, usize),
    /// Bytes that the file does not have.
    Outside,
}

/// Reads `bytes=A-` or `bytes=A-B`.
fn parse_range(range: &str, size: usize) -> Option<Asked> {
    let (first, last) = range.strip_prefix("bytes=")?.split_once('-')?;
    let first: usize = first.parse().ok()?;
    let last = match last {
        "" => size.checked_sub(1),
        last => last
            .parse()
            .ok()
            .map(|last: usize| last.min(size.saturating_sub(1))),
    };
    let within = last.filter(|&last| first <= last && first < size);
    Some(within.map_or(Asked::Outside, |last| Asked::Part(first, last)))
}
