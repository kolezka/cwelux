//! Drives `RemoteHostsStore::list` against canned HTTP servers on 127.0.0.1.

use cwelux_lib::remote::{HostState, RemoteHostStatus, RemoteHostsStore, FILE_NAME};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "test-token-0123456789";
const TIMEOUT: Duration = Duration::from_millis(600);
const VALID_BODY: &str = r#"{"protocol":1,"host":"local-arch","capturedAt":1761440000,"sessions":[{"id":"$3","name":"work","windows":2,"attached":1,"created":1761400000}]}"#;
static COUNTER: AtomicUsize = AtomicUsize::new(0);

enum Reply {
    Respond { status: &'static str, body: String },
    Hang,
}

/// Serves one connection with a canned reply and sends back the raw request head.
fn serve(reply: Reply) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let head = read_head(&mut stream);
        let _ = tx.send(head);
        match reply {
            Reply::Respond { status, body } => {
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
            // Holds the socket open well past the client timeout without answering.
            Reply::Hang => thread::sleep(Duration::from_secs(10)),
        }
    });
    (url, rx)
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn respond(status: &'static str, body: &str) -> (String, mpsc::Receiver<String>) {
    serve(Reply::Respond {
        status,
        body: body.into(),
    })
}

fn store(hosts: &[(&str, &str)]) -> RemoteHostsStore {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("cwelux-remote-it-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let entries: Vec<_> = hosts
        .iter()
        .map(|(name, url)| serde_json::json!({ "name": name, "url": url, "token": TOKEN }))
        .collect();
    let path = dir.join(FILE_NAME);
    fs::write(&path, serde_json::json!({ "hosts": entries }).to_string()).unwrap();
    RemoteHostsStore::with_timeout(Ok(path), TIMEOUT)
}

fn only(statuses: Vec<RemoteHostStatus>) -> RemoteHostStatus {
    assert_eq!(statuses.len(), 1, "{statuses:?}");
    statuses.into_iter().next().unwrap()
}

fn offline_reason(status: &RemoteHostStatus) -> &str {
    assert_eq!(status.state, HostState::Offline, "{status:?}");
    assert!(status.sessions.is_empty());
    let reason = status.error.as_deref().unwrap();
    assert!(!reason.contains(TOKEN), "token leaked: {reason}");
    reason
}

#[test]
fn online_host_sends_the_token_and_returns_sessions() {
    let (url, requests) = respond("200 OK", VALID_BODY);
    let status = only(store(&[("arch", &url)]).list().unwrap());

    let head = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("GET /v1/sessions HTTP/1.1\r\n"), "{head}");
    assert!(
        lower.contains(&format!(
            "authorization: bearer {}\r\n",
            TOKEN.to_ascii_lowercase()
        )),
        "{head}"
    );
    assert!(lower.contains("accept: application/json\r\n"), "{head}");

    assert_eq!(status.state, HostState::Online, "{status:?}");
    assert_eq!(status.name, "arch");
    assert_eq!(status.url, url);
    assert_eq!(status.error, None);
    assert_eq!(status.reported_host.as_deref(), Some("local-arch"));
    assert_eq!(status.captured_at, Some(1761440000));
    assert_eq!(status.sessions.len(), 1);
    assert_eq!(status.sessions[0].id, "$3");
    assert_eq!(status.sessions[0].name, "work");
}

#[test]
fn rejected_token_server_error_and_bad_json_are_offline() {
    let (unauthorized, _a) = respond("401 Unauthorized", r#"{"error":"unauthorized"}"#);
    let (failing, _b) = respond("500 Internal Server Error", r#"{"error":"tmux broke"}"#);
    let (malformed, _c) = respond("200 OK", r#"{"protocol":1,"host":"#);
    let (future, _d) = respond(
        "200 OK",
        &VALID_BODY.replace(r#""protocol":1"#, r#""protocol":2"#),
    );
    let statuses = store(&[
        ("a", &unauthorized),
        ("b", &failing),
        ("c", &malformed),
        ("d", &future),
    ])
    .list()
    .unwrap();

    let names: Vec<_> = statuses.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c", "d"], "config order is kept");
    assert_eq!(offline_reason(&statuses[0]), "token rejected");
    assert_eq!(offline_reason(&statuses[1]), "HTTP 500");
    assert!(offline_reason(&statuses[2]).starts_with("bad response"));
    assert_eq!(offline_reason(&statuses[3]), "unsupported protocol 2");
}

#[test]
fn refused_connection_is_offline_with_a_reason() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let status = only(store(&[("gone", &url)]).list().unwrap());
    let reason = offline_reason(&status).to_ascii_lowercase();
    assert!(reason.contains("refused"), "{reason}");
    assert!(!reason.contains("127.0.0.1"), "url leaked into {reason}");
}

#[test]
fn silent_host_times_out_without_delaying_the_others() {
    let (silent, _a) = serve(Reply::Hang);
    let (fast, _b) = respond("200 OK", VALID_BODY);
    let (also_silent, _c) = serve(Reply::Hang);
    let started = Instant::now();
    let statuses = store(&[
        ("silent", &silent),
        ("fast", &fast),
        ("quiet", &also_silent),
    ])
    .list()
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(offline_reason(&statuses[0]), "timed out after 600 ms");
    assert_eq!(statuses[1].state, HostState::Online, "{:?}", statuses[1]);
    assert_eq!(offline_reason(&statuses[2]), "timed out after 600 ms");
    // Sequential fetches would take two full timeouts.
    assert!(elapsed >= TIMEOUT, "{elapsed:?}");
    assert!(
        elapsed < TIMEOUT * 2,
        "hosts were not fetched concurrently: {elapsed:?}"
    );
}
