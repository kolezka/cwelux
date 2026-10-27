//! HTTP front end: one read-only route, bearer token auth, JSON responses.
//!
//! A minimal HTTP/1.1 server on `std::net`. Every connection carries one request and is
//! closed after the reply. Connections, request heads, read time and tmux calls are bounded.

use crate::tmux::{Session, Tmux, TmuxConfig};
use serde::Serialize;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PROTOCOL: u32 = 1;
pub const SESSIONS_PATH: &str = "/v1/sessions";

const KNOWN_METHODS: [&str; 9] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
];
const LOG_LABEL_MAX_CHARS: usize = 120;
/// After replying, read and discard leftover input for this long so the close is not a reset.
const LINGER: Duration = Duration::from_millis(200);
const LINGER_MAX_BYTES: usize = 64 * 1024;

/// Looks up tmux for each request, so a tmux installed after start is picked up.
pub type TmuxResolver = Box<dyn Fn() -> Result<TmuxConfig, String> + Send + Sync>;

pub struct AgentConfig {
    pub token: String,
    pub host: String,
    pub tmux: TmuxResolver,
    pub limits: Limits,
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Total time a client gets to send its request head. Also the write timeout and the
    /// longest a request waits for its turn to run tmux.
    pub read_timeout: Duration,
    pub max_head_bytes: usize,
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(5),
            max_head_bytes: 8192,
            max_connections: 16,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionsBody<'a> {
    protocol: u32,
    host: &'a str,
    captured_at: u64,
    sessions: Vec<Session>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

pub struct Agent {
    listener: TcpListener,
    shared: Arc<Shared>,
}

struct Shared {
    config: AgentConfig,
    active: AtomicUsize,
    tmux_lock: Mutex<()>,
}

/// Counts one live connection; the count drops when the connection thread ends.
struct ConnectionGuard(Arc<Shared>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            body: error_json(message),
        }
    }
}

enum HeadError {
    /// Timeout, EOF or I/O error before the head ended. Closed without a reply.
    Incomplete,
    TooLarge,
}

struct RequestHead<'a> {
    method: &'a str,
    target: &'a str,
    authorization: Option<&'a str>,
}

impl Agent {
    pub fn bind(listen: &str, config: AgentConfig) -> Result<Self, String> {
        let listener =
            TcpListener::bind(listen).map_err(|e| format!("could not listen on {listen}: {e}"))?;
        let shared = Arc::new(Shared {
            config,
            active: AtomicUsize::new(0),
            tmux_lock: Mutex::new(()),
        });
        Ok(Self { listener, shared })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .expect("a bound listener has a local address")
    }

    /// Connections being served right now, counted until their thread finishes.
    pub fn connection_count(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// Accepts connections until the process exits. Each one gets its own thread, up to the cap.
    pub fn serve(&self) {
        let max = self.shared.config.limits.max_connections;
        for stream in self.listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    // Usually out of file descriptors. Back off instead of spinning.
                    eprintln!("could not accept a connection: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            // Only this thread increments, so check then add cannot overshoot.
            if self.shared.active.load(Ordering::SeqCst) >= max {
                reject_busy(stream);
                continue;
            }
            self.shared.active.fetch_add(1, Ordering::SeqCst);
            let guard = ConnectionGuard(Arc::clone(&self.shared));
            let spawned = std::thread::Builder::new()
                .name("cwelux-conn".into())
                .spawn(move || handle_connection(&guard.0, stream));
            if let Err(e) = spawned {
                // The closure, and with it the guard and the stream, is dropped here.
                eprintln!("could not start a connection thread: {e}");
            }
        }
    }
}

/// Runs on the accept thread, so it never waits on the client.
fn reject_busy(mut stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
    let reply = Reply::error(503, "busy");
    let _ = stream.write_all(&encode(&reply));
    let _ = stream.shutdown(Shutdown::Write);
    // Drop whatever already arrived without waiting for more, so the close is less likely a reset.
    if stream.set_nonblocking(true).is_ok() {
        let mut scratch = [0u8; 4096];
        while matches!(stream.read(&mut scratch), Ok(n) if n > 0) {}
    }
    eprintln!("? ? 503 0ms");
}

fn handle_connection(shared: &Shared, mut stream: TcpStream) {
    let limits = &shared.config.limits;
    let _ = stream.set_write_timeout(Some(limits.read_timeout));
    let started = Instant::now();
    let head = match read_head(&mut stream, limits) {
        Ok(head) => head,
        Err(HeadError::Incomplete) => return,
        Err(HeadError::TooLarge) => {
            let reply = Reply::error(431, "request header too large");
            finish(&mut stream, &reply, None, None, started);
            return;
        }
    };
    match parse_head(&head) {
        Some(request) => {
            let path = request.target.split('?').next().unwrap_or_default();
            let reply = route(shared, &request, path);
            finish(
                &mut stream,
                &reply,
                Some(request.method),
                Some(path),
                started,
            );
        }
        None => {
            let reply = Reply::error(400, "bad request");
            finish(&mut stream, &reply, None, None, started);
        }
    }
}

/// Reads until the blank line that ends the head. The read timeout covers the whole head,
/// so a client that sends one byte at a time cannot hold the connection open longer.
fn read_head(stream: &mut TcpStream, limits: &Limits) -> Result<Vec<u8>, HeadError> {
    let deadline = Instant::now() + limits.read_timeout;
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return Err(HeadError::Incomplete);
        }
        let room = limits.max_head_bytes + 1 - head.len();
        let take = room.min(chunk.len());
        let n = match stream.read(&mut chunk[..take]) {
            Ok(0) => return Err(HeadError::Incomplete),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(HeadError::Incomplete),
        };
        // Search only the new bytes plus the three before them.
        let from = head.len().saturating_sub(3);
        head.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_head_end(&head[from..]) {
            head.truncate(from + end);
            return Ok(head);
        }
        if head.len() > limits.max_head_bytes {
            return Err(HeadError::TooLarge);
        }
    }
}

/// Offset just past `\r\n\r\n`, if present.
fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
}

/// Parses `METHOD SP TARGET SP HTTP/1.x` and picks out the Authorization header.
fn parse_head(head: &[u8]) -> Option<RequestHead<'_>> {
    let mut lines = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let request_line = std::str::from_utf8(lines.next()?).ok()?;
    let mut parts = request_line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || target.is_empty()
        || !target.bytes().all(|b| b.is_ascii_graphic())
        || !version
            .strip_prefix("HTTP/1.")
            .is_some_and(|minor| minor.len() == 1 && minor.as_bytes()[0].is_ascii_digit())
    {
        return None;
    }
    let authorization = lines
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("Authorization"))
        .map(|(_, value)| value.trim());
    Some(RequestHead {
        method,
        target,
        authorization,
    })
}

fn route(shared: &Shared, request: &RequestHead<'_>, path: &str) -> Reply {
    if path != SESSIONS_PATH {
        return Reply::error(404, "not found");
    }
    if request.method != "GET" {
        return Reply::error(405, "method not allowed");
    }
    let authorized = request
        .authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|given| tokens_match(given.as_bytes(), shared.config.token.as_bytes()));
    if !authorized {
        return Reply::error(401, "unauthorized");
    }
    let Some(_turn) = wait_for_tmux(shared) else {
        return Reply::error(503, "busy");
    };
    match sessions_json(&shared.config) {
        Ok(body) => Reply { status: 200, body },
        Err(e) => Reply::error(500, &e),
    }
}

/// One tmux call at a time. Gives up after `read_timeout` so waiting requests do not pile up.
fn wait_for_tmux(shared: &Shared) -> Option<MutexGuard<'_, ()>> {
    let deadline = Instant::now() + shared.config.limits.read_timeout;
    loop {
        match shared.tmux_lock.try_lock() {
            Ok(guard) => return Some(guard),
            // The lock guards no data, so a panic in an earlier call leaves nothing broken.
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() >= deadline => return None,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn sessions_json(config: &AgentConfig) -> Result<String, String> {
    let sessions = Tmux::new((config.tmux)()?).list_sessions()?;
    let body = SessionsBody {
        protocol: PROTOCOL,
        host: &config.host,
        captured_at: unix_now(),
        sessions,
    };
    serde_json::to_string(&body).map_err(|e| format!("could not encode response: {e}"))
}

/// Writes the reply, closes the connection and logs one line. Header values are never logged.
fn finish(
    stream: &mut TcpStream,
    reply: &Reply,
    method: Option<&str>,
    path: Option<&str>,
    started: Instant,
) {
    if let Err(e) = stream.write_all(&encode(reply)) {
        eprintln!("could not send response: {e}");
    }
    let method = method.filter(|m| KNOWN_METHODS.contains(m)).unwrap_or("?");
    let path = path.map(log_label).unwrap_or_else(|| "-".into());
    eprintln!(
        "{method} {path} {} {}ms",
        reply.status,
        started.elapsed().as_millis()
    );
    linger_close(stream);
}

/// Sends FIN, then reads and discards what the client still sends for a short while.
/// Closing with unread input makes the kernel send a reset, which can destroy the reply.
fn linger_close(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Instant::now() + LINGER;
    let mut scratch = [0u8; 4096];
    let mut drained = 0;
    while drained < LINGER_MAX_BYTES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return;
        }
        match stream.read(&mut scratch) {
            Ok(0) | Err(_) => return,
            Ok(n) => drained += n,
        }
    }
}

fn encode(reply: &Reply) -> Vec<u8> {
    let allow = if reply.status == 405 {
        "Allow: GET\r\n"
    } else {
        ""
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{allow}Connection: close\r\n\r\n{}",
        reply.status,
        reason(reply.status),
        reply.body.len(),
        reply.body
    )
    .into_bytes()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// Makes client-supplied text safe for one log line: printable ASCII only, at most 120 chars.
pub fn log_label(text: &str) -> String {
    text.chars()
        .take(LOG_LABEL_MAX_CHARS)
        .map(|c| if matches!(c, ' '..='~') { c } else { '?' })
        .collect()
}

/// Lengths may leak; the bytes are always all compared so timing does not show where they differ.
pub fn tokens_match(given: &[u8], expected: &[u8]) -> bool {
    if given.len() != expected.len() {
        return false;
    }
    let diff = given
        .iter()
        .zip(expected)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(diff) == 0
}

fn error_json(message: &str) -> String {
    serde_json::to_string(&ErrorBody { error: message })
        .unwrap_or_else(|_| r#"{"error":"internal error"}"#.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(tokens_match(b"abcdefghijklmnop", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"abcdefghijklmnoq", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"bbcdefghijklmnop", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"abcdefghijklmno", b"abcdefghijklmnop"));
        assert!(!tokens_match(b"", b"abcdefghijklmnop"));
    }

    #[test]
    fn error_json_escapes_message() {
        assert_eq!(
            error_json("tmux failed: \"x\"\n"),
            r#"{"error":"tmux failed: \"x\"\n"}"#
        );
    }

    #[test]
    fn log_label_keeps_only_printable_ascii() {
        assert_eq!(log_label("/v1/sessions"), "/v1/sessions");
        assert_eq!(log_label("/a b~"), "/a b~");
        assert_eq!(log_label("/missing\nFORGED-LINE"), "/missing?FORGED-LINE");
        assert_eq!(log_label("/x\r\n"), "/x??");
        assert_eq!(log_label("/\x1b[31mred"), "/?[31mred");
        assert_eq!(log_label("/a\tb"), "/a?b");
        assert_eq!(log_label("/\x7f\x00"), "/??");
        // One `?` per character, not per byte.
        assert_eq!(log_label("/zażółć"), "/za????");
        assert_eq!(log_label("/\u{202e}evil"), "/?evil");
    }

    #[test]
    fn log_label_truncates_to_120_chars() {
        let long = format!("/{}", "a".repeat(299));
        let label = log_label(&long);
        assert_eq!(label.chars().count(), 120);
        assert!(long.starts_with(&label));
        let long_non_ascii = "ł".repeat(300);
        assert_eq!(log_label(&long_non_ascii), "?".repeat(120));
    }

    #[test]
    fn parses_request_line_and_authorization() {
        let head = b"GET /v1/sessions?x=1 HTTP/1.1\r\nHost: a\r\nauthorization:  Bearer t \r\n\r\n";
        let request = parse_head(head).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/v1/sessions?x=1");
        assert_eq!(request.authorization, Some("Bearer t"));
        let no_auth = parse_head(b"GET / HTTP/1.0\r\nHost: a\r\n\r\n").unwrap();
        assert_eq!(no_auth.authorization, None);
    }

    #[test]
    fn rejects_malformed_request_lines() {
        for head in [
            &b"GARBAGE\r\n\r\n"[..],
            b"GET /v1/sessions\r\n\r\n",
            b"GET /v1/sessions HTTP/2.0\r\n\r\n",
            b"GET /v1/sessions HTTP/1.1 extra\r\n\r\n",
            b"GET  /v1/sessions HTTP/1.1\r\n\r\n",
            b"get /v1/sessions HTTP/1.1\r\n\r\n",
            b"GET /missing\nFORGED HTTP/1.1\r\n\r\n",
            b"GET /a\x1bb HTTP/1.1\r\n\r\n",
            b"GET /\xff HTTP/1.1\r\n\r\n",
        ] {
            assert!(
                parse_head(head).is_none(),
                "{:?}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn finds_head_end() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(18));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
