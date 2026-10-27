//! Drives the HTTP boundary with fake tmux binaries, the real binary for startup checks,
//! and a private live tmux server when one is installed.

use cwelux_agent::process::run_bounded;
use cwelux_agent::server::{Agent, AgentConfig, Limits};
use cwelux_agent::tmux::TmuxConfig;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: &str = "test-token-0123456789";
const LIVE_TMUX: &str = "/opt/homebrew/bin/tmux";
static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("cwelux-agent-it-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_tmux(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("tmux");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Starts an agent on an ephemeral port. The serving thread lives until the test process exits.
fn start(tmux: TmuxConfig) -> SocketAddr {
    start_with(tmux, Limits::default()).local_addr()
}

fn start_with(tmux: TmuxConfig, limits: Limits) -> Arc<Agent> {
    let config = AgentConfig {
        token: TOKEN.into(),
        host: "test-host".into(),
        tmux: Box::new(move || Ok(tmux.clone())),
        limits,
    };
    let agent = Arc::new(Agent::bind("127.0.0.1:0", config).unwrap());
    let serving = Arc::clone(&agent);
    std::thread::spawn(move || serving.serve());
    agent
}

fn fake_config(tag: &str, body: &str) -> TmuxConfig {
    let dir = temp_dir(tag);
    TmuxConfig {
        binary: fake_tmux(&dir, body),
        socket: None,
        config_file: None,
        timeout: Duration::from_secs(5),
    }
}

fn start_fake(tag: &str, body: &str) -> SocketAddr {
    start(fake_config(tag, body))
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        assert_eq!(self.header("Content-Type"), Some("application/json"));
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

fn request(addr: SocketAddr, method: &str, path: &str, auth: Option<&str>) -> Reply {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    if let Some(value) = auth {
        head.push_str(&format!("Authorization: {value}\r\n"));
    }
    if method != "GET" {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("\r\n");
    exchange(addr, head.as_bytes())
}

/// Sends raw bytes and parses the reply.
fn exchange(addr: SocketAddr, bytes: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(bytes).unwrap();
    read_reply(&mut stream)
}

fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .expect("response has a header block");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line in {raw:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_string(),
    }
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

const CANNED: &str = r"printf '%s\n' '\$3 work\ dir 2 1 1761400000;'";

#[test]
fn lists_sessions_with_contract_fields() {
    let addr = start_fake("ok", CANNED);
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let json = reply.json();
    assert_eq!(json["protocol"], 1);
    assert_eq!(json["host"], "test-host");
    let captured = json["capturedAt"].as_u64().expect("capturedAt is a number");
    assert!(captured >= before && captured < before + 60);
    assert_eq!(
        json["sessions"],
        serde_json::json!([
            { "id": "$3", "name": "work dir", "windows": 2, "attached": 1, "created": 1761400000u64 }
        ])
    );
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 4, "{keys:?}");
}

#[test]
fn rejects_missing_or_wrong_token() {
    let addr = start_fake("auth", CANNED);
    let wrong = format!("Bearer {TOKEN}x");
    let wrong_same_length = format!("Bearer {}X", &TOKEN[..TOKEN.len() - 1]);
    let lowercase_scheme = format!("bearer {TOKEN}");
    for auth in [
        None,
        Some(wrong.as_str()),
        Some(wrong_same_length.as_str()),
        Some(TOKEN),
        Some(lowercase_scheme.as_str()),
        Some("Bearer "),
    ] {
        let reply = request(addr, "GET", "/v1/sessions", auth);
        assert_eq!(reply.status, 401, "{auth:?}");
        assert_eq!(reply.json(), serde_json::json!({ "error": "unauthorized" }));
    }
}

#[test]
fn other_paths_are_not_found() {
    let addr = start_fake("404", CANNED);
    for path in ["/", "/v1/sessions/", "/v1/session", "/v2/sessions"] {
        let reply = request(addr, "GET", path, Some(&bearer()));
        assert_eq!(reply.status, 404, "{path}");
        assert!(reply.json()["error"].is_string());
    }
}

#[test]
fn other_methods_are_not_allowed() {
    let addr = start_fake("405", CANNED);
    for method in ["POST", "PUT", "DELETE"] {
        let reply = request(addr, method, "/v1/sessions", Some(&bearer()));
        assert_eq!(reply.status, 405, "{method}");
        assert_eq!(reply.header("Allow"), Some("GET"));
        assert!(reply.json()["error"].is_string());
    }
}

#[test]
fn missing_tmux_server_is_an_empty_list() {
    let addr = start_fake(
        "noserver",
        "echo 'no server running on /tmp/tmux-1000/default' >&2; exit 1",
    );
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["sessions"], serde_json::json!([]));
}

#[test]
fn tmux_failure_is_a_server_error() {
    let addr = start_fake(
        "fail",
        "echo 'error connecting to /tmp/x.sock (Permission denied)' >&2; exit 1",
    );
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 500);
    let error = reply.json()["error"].as_str().unwrap().to_string();
    assert!(error.contains("Permission denied"), "{error}");
}

#[test]
fn malformed_tmux_output_is_a_server_error() {
    let addr = start_fake("garbage", "printf 'garbage without terminator\\n'");
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 500);
    assert!(reply.json()["error"]
        .as_str()
        .unwrap()
        .contains("unexpected tmux output"));
}

fn agent_binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cwelux-agent"));
    for key in [
        "CWELUX_AGENT_LISTEN",
        "CWELUX_AGENT_TOKEN_FILE",
        "CWELUX_AGENT_HOST_NAME",
        "CWELUX_TMUX_PATH",
        "CWELUX_TMUX_SOCKET",
    ] {
        command.env_remove(key);
    }
    command
}

#[test]
fn binary_refuses_short_token() {
    let dir = temp_dir("short");
    let token_file = dir.join("token");
    fs::write(&token_file, "short-token-123\n").unwrap();
    let out = agent_binary()
        .args([
            "--listen",
            "127.0.0.1:0",
            "--host-name",
            "x",
            "--token-file",
        ])
        .arg(&token_file)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("shorter than 16"), "{stderr}");
    assert!(
        !stderr.contains("short-token-123"),
        "token leaked: {stderr}"
    );
}

#[test]
fn binary_refuses_missing_token_file() {
    let out = agent_binary()
        .args(["--listen", "127.0.0.1:0", "--host-name", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = agent_binary()
        .args(["--token-file", "/nonexistent/cwelux-token"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// Kills the spawned agent even when an assertion fails.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn binary_serves_with_env_config_and_passes_socket() {
    let dir = temp_dir("bin");
    let token_file = dir.join("token");
    fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let args_log = dir.join("args");
    let body = format!(
        "for a in \"$@\"; do printf '[%s]\\n' \"$a\"; done > '{}'\n{CANNED}",
        args_log.display()
    );
    let binary = fake_tmux(&dir, &body);
    let child = agent_binary()
        .env("CWELUX_AGENT_LISTEN", "127.0.0.1:0")
        .env("CWELUX_AGENT_TOKEN_FILE", &token_file)
        .env("CWELUX_AGENT_HOST_NAME", "env-host")
        .env("CWELUX_TMUX_PATH", &binary)
        .env("CWELUX_TMUX_SOCKET", "/tmp/cwelux-agent-test.sock")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut guard = ChildGuard(child);
    let mut stderr = BufReader::new(guard.0.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let addr: SocketAddr = line
        .trim()
        .strip_prefix("cwelux-agent listening on ")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .parse()
        .unwrap();

    let reply = request(addr, "GET", "/v1/sessions?probe=1", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let json = reply.json();
    assert_eq!(json["host"], "env-host");
    assert_eq!(json["sessions"][0]["id"], "$3");
    let argv = fs::read_to_string(&args_log).unwrap();
    assert!(
        argv.starts_with("[-S]\n[/tmp/cwelux-agent-test.sock]\n[list-sessions]\n[-F]\n"),
        "{argv}"
    );

    line.clear();
    stderr.read_line(&mut line).unwrap();
    let parts: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(&parts[..3], ["GET", "/v1/sessions", "200"], "{line:?}");
    assert!(parts[3].ends_with("ms"), "{line:?}");
    assert!(!line.contains(TOKEN), "token leaked into the log");
}

/// Kills the private tmux server when the test ends, even on panic.
struct LiveServer {
    root: PathBuf,
    socket: PathBuf,
}

impl LiveServer {
    fn new() -> Self {
        // Short root: Unix socket paths are limited to about 104 bytes on macOS.
        let root = PathBuf::from(format!("/tmp/cwxa-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("tmux.sock");
        LiveServer { root, socket }
    }

    fn tmux(&self, args: &[&str]) -> Command {
        let mut command = Command::new(LIVE_TMUX);
        command
            .arg("-S")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .env_remove("TMUX");
        command
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        if let Err(error) =
            run_bounded(self.tmux(&["kill-server"]), Duration::from_secs(5), 1 << 20)
        {
            eprintln!("Could not stop isolated tmux fixture: {error}");
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn live_tmux_session_is_reported() {
    if !Path::new(LIVE_TMUX).exists() {
        eprintln!("skipping live_tmux_session_is_reported: {LIVE_TMUX} not found");
        return;
    }
    let server = LiveServer::new();
    // Goes through run_bounded so it shares the spawn lock with the agent's own tmux calls.
    let created = run_bounded(
        server.tmux(&["new-session", "-d", "-s", "cwx-agent-live"]),
        Duration::from_secs(5),
        1 << 20,
    )
    .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let addr = start(TmuxConfig {
        binary: PathBuf::from(LIVE_TMUX),
        socket: Some(server.socket.clone()),
        config_file: Some(PathBuf::from("/dev/null")),
        timeout: Duration::from_secs(5),
    });
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let sessions = reply.json()["sessions"].as_array().unwrap().clone();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0]["name"], "cwx-agent-live");
    assert!(sessions[0]["id"].as_str().unwrap().starts_with('$'));
    assert_eq!(sessions[0]["windows"], 1);
    assert_eq!(sessions[0]["attached"], 0);
}

fn quick_limits(read_timeout_ms: u64, max_connections: usize) -> Limits {
    Limits {
        read_timeout: Duration::from_millis(read_timeout_ms),
        max_connections,
        ..Limits::default()
    }
}

/// Polls until the agent has `want` live connections, so a test never races the accept thread.
fn wait_for_connections(agent: &Agent, want: usize, within: Duration) {
    let deadline = Instant::now() + within;
    while agent.connection_count() != want {
        assert!(
            Instant::now() < deadline,
            "expected {want} connections, have {}",
            agent.connection_count()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn idle_partial_headers_do_not_hold_the_server() {
    let agent = start_with(fake_config("idle", CANNED), quick_limits(300, 64));
    let addr = agent.local_addr();
    let mut idle: Vec<TcpStream> = (0..20)
        .map(|_| {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(b"GET /v1/sessions HTTP/1.1\r\nHost: x\r\n")
                .unwrap();
            stream
        })
        .collect();
    wait_for_connections(&agent, 20, Duration::from_secs(2));

    let started = Instant::now();
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(started.elapsed() < Duration::from_secs(2));

    std::thread::sleep(Duration::from_millis(300 + 700));
    assert_eq!(
        agent.connection_count(),
        0,
        "idle connections were not closed"
    );
    // The server closed each idle socket without writing anything.
    for stream in &mut idle {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "{:?}", String::from_utf8_lossy(&rest));
    }
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200, "{}", reply.body);
}

#[test]
fn connections_over_the_cap_get_busy() {
    let agent = start_with(fake_config("cap", CANNED), quick_limits(3000, 4));
    let addr = agent.local_addr();
    let _held: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(addr).unwrap()).collect();
    wait_for_connections(&agent, 4, Duration::from_secs(2));

    let started = Instant::now();
    let mut extra = TcpStream::connect(addr).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let reply = read_reply(&mut extra);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(reply.status, 503);
    assert_eq!(reply.header("Connection"), Some("close"));
    assert_eq!(reply.json(), serde_json::json!({ "error": "busy" }));
}

#[test]
fn oversized_head_is_rejected() {
    let addr = start_fake("431", CANNED);
    let mut head = b"GET /v1/sessions HTTP/1.1\r\n".to_vec();
    while head.len() < 9000 {
        head.extend_from_slice(b"X-Filler: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    let reply = exchange(addr, &head);
    assert_eq!(reply.status, 431);
    assert!(reply.json()["error"].is_string());
}

#[test]
fn malformed_request_line_is_bad_request() {
    let addr = start_fake("400", CANNED);
    let reply = exchange(addr, b"GARBAGE\r\n\r\n");
    assert_eq!(reply.status, 400);
    assert!(reply.json()["error"].is_string());
}

#[test]
fn replies_carry_length_and_close() {
    let addr = start_fake("framing", CANNED);
    let reply = request(addr, "GET", "/v1/sessions", Some(&bearer()));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("Connection"), Some("close"));
    assert_eq!(
        reply.header("Content-Length"),
        Some(reply.body.len().to_string().as_str())
    );
}

#[test]
fn tmux_calls_never_overlap() {
    let dir = temp_dir("serial");
    let marker = dir.join("running");
    let overlaps = dir.join("overlaps");
    let body = format!(
        "mkdir '{m}' 2>/dev/null || echo overlap >> '{o}'\nsleep 0.2\nrmdir '{m}' 2>/dev/null\n{CANNED}",
        m = marker.display(),
        o = overlaps.display()
    );
    let tmux = TmuxConfig {
        binary: fake_tmux(&dir, &body),
        socket: None,
        config_file: None,
        timeout: Duration::from_secs(5),
    };
    let addr = start_with(tmux, quick_limits(5000, 16)).local_addr();
    let handles: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(move || request(addr, "GET", "/v1/sessions", Some(&bearer()))))
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap().status, 200);
    }
    assert!(!overlaps.exists(), "tmux ran concurrently");
}

#[test]
fn waiting_too_long_for_tmux_is_busy() {
    let body = format!("sleep 1\n{CANNED}");
    let addr = start_with(fake_config("slow", &body), quick_limits(300, 16)).local_addr();
    let handles: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(move || request(addr, "GET", "/v1/sessions", Some(&bearer()))))
        .collect();
    let mut statuses: Vec<u16> = handles
        .into_iter()
        .map(|h| h.join().unwrap().status)
        .collect();
    statuses.sort();
    assert_eq!(statuses, [200, 503]);
}
