//! HTTP front end: one read-only route, bearer token auth, JSON responses.

use crate::tmux::{Session, Tmux, TmuxConfig};
use serde::Serialize;
use std::net::SocketAddr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response};

pub const PROTOCOL: u32 = 1;
pub const SESSIONS_PATH: &str = "/v1/sessions";

/// Looks up tmux for each request, so a tmux installed after start is picked up.
pub type TmuxResolver = Box<dyn Fn() -> Result<TmuxConfig, String> + Send + Sync>;

pub struct AgentConfig {
    pub token: String,
    pub host: String,
    pub tmux: TmuxResolver,
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
    http: tiny_http::Server,
    config: AgentConfig,
}

impl Agent {
    pub fn bind(listen: &str, config: AgentConfig) -> Result<Self, String> {
        let http = tiny_http::Server::http(listen)
            .map_err(|e| format!("could not listen on {listen}: {e}"))?;
        Ok(Self { http, config })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.http
            .server_addr()
            .to_ip()
            .expect("an HTTP listener always has an IP address")
    }

    /// Serves requests one at a time until the process exits.
    pub fn serve(&self) {
        for request in self.http.incoming_requests() {
            self.handle(request);
        }
    }

    fn handle(&self, request: Request) {
        let started = Instant::now();
        let method = request.method().clone();
        // Drop the query so nothing a client puts there ends up in the log.
        let path = request
            .url()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let (status, body) = self.route(&request, &method, &path);
        let response = Response::from_string(body)
            .with_status_code(status)
            .with_header(header("Content-Type", "application/json"));
        let response = if status == 405 {
            response.with_header(header("Allow", "GET"))
        } else {
            response
        };
        if let Err(e) = request.respond(response) {
            eprintln!("could not send response: {e}");
        }
        eprintln!(
            "{method} {path} {status} {}ms",
            started.elapsed().as_millis()
        );
    }

    fn route(&self, request: &Request, method: &Method, path: &str) -> (u16, String) {
        if path != SESSIONS_PATH {
            return (404, error_json("not found"));
        }
        if *method != Method::Get {
            return (405, error_json("method not allowed"));
        }
        if !self.authorized(request) {
            return (401, error_json("unauthorized"));
        }
        match self.sessions_json() {
            Ok(body) => (200, body),
            Err(e) => (500, error_json(&e)),
        }
    }

    fn authorized(&self, request: &Request) -> bool {
        request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .is_some_and(|given| tokens_match(given.as_bytes(), self.config.token.as_bytes()))
    }

    fn sessions_json(&self) -> Result<String, String> {
        let sessions = Tmux::new((self.config.tmux)()?).list_sessions()?;
        let body = SessionsBody {
            protocol: PROTOCOL,
            host: &self.config.host,
            captured_at: unix_now(),
            sessions,
        };
        serde_json::to_string(&body).map_err(|e| format!("could not encode response: {e}"))
    }
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

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header is valid")
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
}
