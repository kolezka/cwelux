//! Read-only session lists from `cwelux-agent` hosts, polled over HTTP.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::tmux::{validate_session_id, Session};

pub const FILE_NAME: &str = "remote-hosts.json";
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(3);
pub const PROTOCOL: u64 = 1;
const SESSIONS_PATH: &str = "/v1/sessions";
const MAX_NAME_CHARS: usize = 64;
/// The agent caps tmux output at 8 MiB, so a valid reply stays well under this.
const MAX_BODY: u64 = 16 * 1024 * 1024;

/// One configured host. The token never appears in `Debug` output or error strings.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteHost {
    pub name: String,
    /// Normalized: scheme and authority only, no trailing slash.
    pub url: String,
    token: String,
}

impl fmt::Debug for RemoteHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteHost")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl RemoteHost {
    pub fn new(name: &str, url: &str, token: &str) -> Result<Self, String> {
        let count = name.chars().count();
        if count == 0 || count > MAX_NAME_CHARS {
            return Err(format!(
                "name must be 1 to {MAX_NAME_CHARS} characters, got {count}."
            ));
        }
        let url = normalize_url(url)?;
        if token.is_empty() {
            return Err(format!("host {name:?} has an empty token."));
        }
        if HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
            return Err(format!(
                "host {name:?} has a token with characters not allowed in an HTTP header."
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            url,
            token: token.to_owned(),
        })
    }
}

/// Accepts `http(s)://host[:port]` with an optional single `/`, and returns it without the slash.
fn normalize_url(raw: &str) -> Result<String, String> {
    let bad = |why: &str| format!("url {raw:?} {why}");
    let parsed = Url::parse(raw).map_err(|e| bad(&format!("is not a valid URL ({e}).")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(bad("must start with http:// or https://."));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(bad("has no host."));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad("must not contain a user name or password."));
    }
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(bad("must not have a path."));
    }
    if parsed.query().is_some() {
        return Err(bad("must not have a query."));
    }
    if parsed.fragment().is_some() {
        return Err(bad("must not have a fragment."));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    hosts: Vec<HostEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostEntry {
    name: String,
    url: String,
    token: String,
}

/// Parses and validates the config text. Hosts keep file order.
pub fn parse_config(text: &str) -> Result<Vec<RemoteHost>, String> {
    let file: ConfigFile = serde_json::from_str(text).map_err(|e| format!("malformed ({e})."))?;
    let mut seen = HashSet::new();
    file.hosts
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let host = RemoteHost::new(&entry.name, &entry.url, &entry.token)
                .map_err(|e| format!("host {}: {e}", i + 1))?;
            if !seen.insert(host.name.clone()) {
                return Err(format!("host name {:?} is used more than once.", host.name));
            }
            Ok(host)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostState {
    Online,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHostStatus {
    pub name: String,
    pub url: String,
    pub state: HostState,
    pub error: Option<String>,
    pub reported_host: Option<String>,
    pub captured_at: Option<u64>,
    pub sessions: Vec<Session>,
}

impl RemoteHostStatus {
    fn offline(host: &RemoteHost, reason: impl Into<String>) -> Self {
        Self {
            name: host.name.clone(),
            url: host.url.clone(),
            state: HostState::Offline,
            error: Some(reason.into()),
            reported_host: None,
            captured_at: None,
            sessions: Vec::new(),
        }
    }
}

/// A decoded protocol 1 reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub host: String,
    pub captured_at: u64,
    pub sessions: Vec<Session>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WirePayload {
    #[allow(dead_code)]
    protocol: u64,
    host: String,
    captured_at: u64,
    sessions: Vec<WireSession>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireSession {
    id: String,
    name: String,
    windows: u32,
    attached: u32,
    created: u64,
}

/// Strict decoder: the protocol is checked first, then every field and session.
pub fn decode_payload(body: &[u8]) -> Result<Snapshot, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("bad response ({e})"))?;
    match value.get("protocol").and_then(serde_json::Value::as_u64) {
        Some(PROTOCOL) => {}
        Some(other) => return Err(format!("unsupported protocol {other}")),
        None => return Err("bad response (no protocol number)".into()),
    }
    let wire: WirePayload =
        serde_json::from_value(value).map_err(|e| format!("bad response ({e})"))?;
    if wire.host.is_empty() {
        return Err("bad response (empty host)".into());
    }
    let sessions = wire
        .sessions
        .into_iter()
        .map(|s| {
            validate_session_id(&s.id).map_err(|e| format!("bad response ({e})"))?;
            if s.name.is_empty() {
                return Err(format!("bad response (session {} has no name)", s.id));
            }
            Ok(Session {
                id: s.id,
                name: s.name,
                windows: s.windows,
                attached: s.attached,
                created: s.created,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Snapshot {
        host: wire.host,
        captured_at: wire.captured_at,
        sessions,
    })
}

/// What one request produced, before it is turned into a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    Response { status: u16, body: Vec<u8> },
    TimedOut(Duration),
    Failed(String),
}

/// Pure mapping from a request outcome to the status the UI shows.
pub fn status_from(host: &RemoteHost, outcome: FetchOutcome) -> RemoteHostStatus {
    match outcome {
        FetchOutcome::Response { status: 200, body } => match decode_payload(&body) {
            Ok(snapshot) => RemoteHostStatus {
                name: host.name.clone(),
                url: host.url.clone(),
                state: HostState::Online,
                error: None,
                reported_host: Some(snapshot.host),
                captured_at: Some(snapshot.captured_at),
                sessions: snapshot.sessions,
            },
            Err(e) => RemoteHostStatus::offline(host, e),
        },
        FetchOutcome::Response { status: 401, .. } => {
            RemoteHostStatus::offline(host, "token rejected")
        }
        FetchOutcome::Response { status, .. } => {
            RemoteHostStatus::offline(host, format!("HTTP {status}"))
        }
        FetchOutcome::TimedOut(after) => {
            RemoteHostStatus::offline(host, format!("timed out after {}", describe(after)))
        }
        FetchOutcome::Failed(reason) => RemoteHostStatus::offline(host, reason),
    }
}

fn describe(duration: Duration) -> String {
    if duration.subsec_millis() == 0 {
        format!("{} s", duration.as_secs())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

/// The innermost cause, without the request URL. The token is only ever in a header.
fn reason(error: reqwest::Error, timeout: Duration) -> FetchOutcome {
    if error.is_timeout() {
        return FetchOutcome::TimedOut(timeout);
    }
    let error = error.without_url();
    let mut cause: &dyn std::error::Error = &error;
    while let Some(next) = cause.source() {
        cause = next;
    }
    FetchOutcome::Failed(cause.to_string())
}

/// One blocking request. Call it off any async runtime thread.
pub fn fetch(host: &RemoteHost, timeout: Duration) -> FetchOutcome {
    // Stage 1 builds reqwest without TLS, so https URLs pass validation but fail here.
    // Serving https needs a TLS feature on reqwest in Cargo.toml.
    let client = match Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(Policy::none())
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(e) => return reason(e, timeout),
    };
    let mut auth = match HeaderValue::from_str(&format!("Bearer {}", host.token)) {
        Ok(value) => value,
        Err(_) => return FetchOutcome::Failed("token is not a valid header value".into()),
    };
    auth.set_sensitive(true);
    let response = match client
        .get(format!("{}{SESSIONS_PATH}", host.url))
        .header(AUTHORIZATION, auth)
        .header(ACCEPT, "application/json")
        .send()
    {
        Ok(response) => response,
        Err(e) => return reason(e, timeout),
    };
    let status = response.status().as_u16();
    if status != 200 {
        return FetchOutcome::Response {
            status,
            body: Vec::new(),
        };
    }
    let mut body = Vec::new();
    if let Err(e) = response.take(MAX_BODY + 1).read_to_end(&mut body) {
        let timed_out = e.kind() == std::io::ErrorKind::TimedOut
            || e.get_ref()
                .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
                .is_some_and(reqwest::Error::is_timeout);
        return if timed_out {
            FetchOutcome::TimedOut(timeout)
        } else {
            FetchOutcome::Failed(format!("could not read the response ({e})"))
        };
    }
    if body.len() as u64 > MAX_BODY {
        return FetchOutcome::Failed("response is larger than 16 MiB".into());
    }
    FetchOutcome::Response { status, body }
}

/// Fetches every host on its own thread, so one slow host never delays the others.
/// Plain threads keep reqwest's blocking client away from the Tauri async runtime.
pub fn fetch_all(hosts: &[RemoteHost], timeout: Duration) -> Vec<RemoteHostStatus> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = hosts
            .iter()
            .map(|host| scope.spawn(move || status_from(host, fetch(host, timeout))))
            .collect();
        handles
            .into_iter()
            .zip(hosts)
            .map(|(handle, host)| {
                handle
                    .join()
                    .unwrap_or_else(|_| RemoteHostStatus::offline(host, "internal error"))
            })
            .collect()
    })
}

/// Reads `remote-hosts.json` on every call, so hand edits apply on the next refresh.
#[derive(Debug, Clone)]
pub struct RemoteHostsStore {
    path: Result<PathBuf, String>,
    timeout: Duration,
}

impl RemoteHostsStore {
    pub fn new(path: Result<PathBuf, String>) -> Self {
        Self::with_timeout(path, FETCH_TIMEOUT)
    }

    pub fn with_timeout(path: Result<PathBuf, String>, timeout: Duration) -> Self {
        Self { path, timeout }
    }

    /// A missing file means no hosts. Anything unreadable, malformed or invalid is an error.
    pub fn load(&self) -> Result<Vec<RemoteHost>, String> {
        let path = self
            .path
            .as_deref()
            .map_err(|e| format!("Remote hosts location is unavailable: {e}"))?;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "Could not read remote hosts from {}: {e}",
                    path.display()
                ))
            }
        };
        parse_config(&text)
            .map_err(|e| format!("Remote hosts file {} is invalid: {e}", path.display()))
    }

    /// Statuses in config order. Blocks for up to the timeout.
    pub fn list(&self) -> Result<Vec<RemoteHostStatus>, String> {
        Ok(fetch_all(&self.load()?, self.timeout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const TOKEN: &str = "s3cret-token-value-1234";

    fn host() -> RemoteHost {
        RemoteHost::new("arch", "http://100.64.0.5:7788", TOKEN).unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cwelux-remote-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_with(dir: &Path, body: Option<&str>) -> RemoteHostsStore {
        let path = dir.join(FILE_NAME);
        if let Some(body) = body {
            fs::write(&path, body).unwrap();
        }
        RemoteHostsStore::new(Ok(path))
    }

    fn config(entries: &str) -> String {
        format!(r#"{{"hosts":[{entries}]}}"#)
    }

    fn entry(name: &str, url: &str, token: &str) -> String {
        format!(r#"{{"name":"{name}","url":"{url}","token":"{token}"}}"#)
    }

    const VALID_BODY: &str = r#"{"protocol":1,"host":"local-arch","capturedAt":1761440000,
        "sessions":[{"id":"$3","name":"work","windows":2,"attached":1,"created":1761400000}]}"#;

    #[test]
    fn valid_file_keeps_order_and_normalizes_urls() {
        let dir = temp_dir("valid");
        let body = config(&format!(
            "{},{}",
            entry("local-arch", "http://100.64.0.5:7788/", "abc"),
            entry("nas", "https://NAS.example:443", "def")
        ));
        let hosts = store_with(&dir, Some(&body)).load().unwrap();
        let names: Vec<_> = hosts.iter().map(|h| h.name.as_str()).collect();
        let urls: Vec<_> = hosts.iter().map(|h| h.url.as_str()).collect();
        assert_eq!(names, ["local-arch", "nas"]);
        assert_eq!(urls, ["http://100.64.0.5:7788", "https://nas.example"]);
    }

    #[test]
    fn missing_file_means_no_hosts_but_unreadable_is_an_error() {
        let dir = temp_dir("missing");
        assert_eq!(store_with(&dir, None).load().unwrap(), Vec::new());
        assert_eq!(store_with(&dir, None).list().unwrap(), Vec::new());

        let unreadable = temp_dir("unreadable");
        fs::create_dir(unreadable.join(FILE_NAME)).unwrap();
        let err = store_with(&unreadable, None).load().unwrap_err();
        assert!(err.contains("Could not read remote hosts"), "{err}");
        assert!(err.contains(FILE_NAME), "{err}");
    }

    #[test]
    fn unavailable_location_is_an_error() {
        let err = RemoteHostsStore::new(Err("no config dir".into()))
            .list()
            .unwrap_err();
        assert!(err.contains("no config dir"), "{err}");
    }

    #[test]
    fn invalid_files_are_errors_naming_the_file_and_never_the_token() {
        let good = entry("a", "http://h:1", TOKEN);
        let cases = [
            "not json".to_string(),
            String::new(),
            "{}".to_string(),
            r#"{"hosts":{}}"#.to_string(),
            format!(r#"{{"hosts":[{good}],"extra":1}}"#),
            config(&format!(
                r#"{{"name":"a","url":"http://h:1","token":"{TOKEN}","port":1}}"#
            )),
            config(r#"{"name":"a","url":"http://h:1"}"#),
            config(&format!("{good},{}", entry("a", "http://other:2", TOKEN))),
            config(&entry("", "http://h:1", TOKEN)),
            config(&entry(&"x".repeat(65), "http://h:1", TOKEN)),
            config(&entry("a", "http://h:1", "")),
            config(&entry("a", "http://h:1", "bad\\ntoken")),
            config(&entry("a", "ftp://h:1", TOKEN)),
            config(&entry("a", "h:1", TOKEN)),
            config(&entry("a", "100.64.0.5:7788", TOKEN)),
            config(&entry("a", "http://", TOKEN)),
            config(&entry("a", "http://h:1/v1", TOKEN)),
            config(&entry("a", "http://h:1//", TOKEN)),
            config(&entry("a", "http://h:1?x=1", TOKEN)),
            config(&entry("a", "http://h:1#top", TOKEN)),
            config(&entry("a", "http://user:pw@h:1", TOKEN)),
            config(&entry("a", "", TOKEN)),
        ];
        for body in cases {
            let dir = temp_dir("invalid");
            let err = store_with(&dir, Some(&body)).load().expect_err(&body);
            assert!(err.contains(FILE_NAME), "{body}: {err}");
            assert!(!err.contains(TOKEN), "{body}: token leaked in {err}");
        }
    }

    #[test]
    fn name_limit_counts_characters_not_bytes() {
        assert!(RemoteHost::new(&"ż".repeat(64), "http://h", "t").is_ok());
        assert!(RemoteHost::new(&"ż".repeat(65), "http://h", "t").is_err());
    }

    #[test]
    fn debug_output_redacts_the_token() {
        let text = format!("{:?}", host());
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("redacted"), "{text}");
    }

    #[test]
    fn decodes_a_valid_payload_strictly() {
        let snapshot = decode_payload(VALID_BODY.as_bytes()).unwrap();
        assert_eq!(snapshot.host, "local-arch");
        assert_eq!(snapshot.captured_at, 1761440000);
        assert_eq!(
            snapshot.sessions,
            vec![Session {
                id: "$3".into(),
                name: "work".into(),
                windows: 2,
                attached: 1,
                created: 1761400000,
            }]
        );
        let empty = r#"{"protocol":1,"host":"h","capturedAt":1,"sessions":[]}"#;
        assert!(decode_payload(empty.as_bytes())
            .unwrap()
            .sessions
            .is_empty());
    }

    #[test]
    fn rejects_bad_payloads() {
        let session = r#"{"id":"$3","name":"work","windows":2,"attached":1,"created":5}"#;
        let with = |protocol: &str, host: &str, sessions: &str| {
            format!(
                r#"{{"protocol":{protocol},"host":"{host}","capturedAt":1,"sessions":[{sessions}]}}"#
            )
        };
        let cases = [
            (with("2", "h", session), "unsupported protocol 2"),
            (with("0", "h", session), "unsupported protocol 0"),
            (with("\"1\"", "h", session), "no protocol"),
            (
                r#"{"host":"h","capturedAt":1,"sessions":[]}"#.into(),
                "no protocol",
            ),
            (with("1", "", session), "empty host"),
            (
                with("1", "h", &session.replace("$3", "3")),
                "Invalid tmux session id",
            ),
            (
                with("1", "h", &session.replace("$3", "$12345678901")),
                "Invalid tmux session id",
            ),
            (
                with("1", "h", &session.replace("\"work\"", "\"\"")),
                "has no name",
            ),
            (
                with(
                    "1",
                    "h",
                    &session.replace("\"windows\":2", "\"windows\":-1"),
                ),
                "bad response",
            ),
            (
                with(
                    "1",
                    "h",
                    &session.replace("\"created\":5", "\"created\":5,\"x\":1"),
                ),
                "unknown field",
            ),
            (
                r#"{"protocol":1,"host":"h","capturedAt":1,"sessions":[],"x":1}"#.into(),
                "unknown field",
            ),
            (
                r#"{"protocol":1,"host":"h","sessions":[]}"#.into(),
                "missing field",
            ),
            ("[]".into(), "no protocol"),
            ("{\"protocol\":1".into(), "bad response"),
            ("<html>".into(), "bad response"),
        ];
        for (body, wanted) in cases {
            let err = decode_payload(body.as_bytes()).expect_err(&body);
            assert!(err.contains(wanted), "{body}: {err}");
        }
    }

    #[test]
    fn maps_outcomes_to_statuses() {
        let host = host();
        let online = status_from(
            &host,
            FetchOutcome::Response {
                status: 200,
                body: VALID_BODY.as_bytes().to_vec(),
            },
        );
        assert_eq!(online.state, HostState::Online);
        assert_eq!(online.error, None);
        assert_eq!(online.reported_host.as_deref(), Some("local-arch"));
        assert_eq!(online.captured_at, Some(1761440000));
        assert_eq!(online.sessions.len(), 1);
        assert_eq!(online.url, "http://100.64.0.5:7788");

        let offline = |outcome| {
            let status = status_from(&host, outcome);
            assert_eq!(status.state, HostState::Offline);
            assert_eq!(status.reported_host, None);
            assert_eq!(status.captured_at, None);
            assert!(status.sessions.is_empty());
            status.error.unwrap()
        };
        let response = |status: u16, body: &str| FetchOutcome::Response {
            status,
            body: body.as_bytes().to_vec(),
        };
        assert_eq!(offline(response(401, "")), "token rejected");
        assert_eq!(offline(response(500, "{}")), "HTTP 500");
        assert_eq!(offline(response(404, "")), "HTTP 404");
        assert_eq!(offline(response(302, "")), "HTTP 302");
        assert!(offline(response(200, "nope")).starts_with("bad response"));
        assert_eq!(
            offline(FetchOutcome::TimedOut(FETCH_TIMEOUT)),
            "timed out after 3 s"
        );
        assert_eq!(
            offline(FetchOutcome::TimedOut(Duration::from_millis(250))),
            "timed out after 250 ms"
        );
        assert_eq!(
            offline(FetchOutcome::Failed("connection refused".into())),
            "connection refused"
        );
    }

    #[test]
    fn status_serializes_in_the_contract_shape() {
        let status = status_from(&host(), FetchOutcome::Failed("connection refused".into()));
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "name": "arch",
                "url": "http://100.64.0.5:7788",
                "state": "offline",
                "error": "connection refused",
                "reportedHost": null,
                "capturedAt": null,
                "sessions": []
            })
        );
    }
}
