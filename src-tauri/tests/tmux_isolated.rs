//! Live tests against a private tmux server on a temp socket, plus fake-binary fixtures
//! that drive the public read boundary. Never touches the user's default server.

use cwelux_lib::process::{run_bounded, CommandOutput};
use cwelux_lib::tmux::{Tmux, TmuxConfig};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const TMUX: &str = "/opt/homebrew/bin/tmux";
static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cwelux-it-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(binary: PathBuf, socket: PathBuf) -> TmuxConfig {
    TmuxConfig {
        binary,
        socket: Some(socket),
        config_file: Some(PathBuf::from("/dev/null")),
        path_env: OsString::from("/opt/homebrew/bin:/usr/bin:/bin"),
        timeout: Duration::from_secs(5),
    }
}

/// Kills the private server when the test ends, even on panic.
struct Server {
    root: PathBuf,
    socket: PathBuf,
}

impl Server {
    fn new(tag: &str) -> Self {
        // Short root: Unix socket paths are limited to ~104 bytes on macOS.
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = PathBuf::from(format!("/tmp/cwx-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("tmux.sock");
        Server { root, socket }
    }

    fn tmux(&self) -> Tmux {
        Tmux::new(config(PathBuf::from(TMUX), self.socket.clone()))
    }

    /// Goes through `run_bounded` so its spawns share the library's spawn lock; a plain
    /// `Command::output` here could leak another test's pipe into a server this starts.
    fn raw(&self, args: &[&str]) -> CommandOutput {
        let mut command = Command::new(TMUX);
        command
            .arg("-S")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .env_remove("TMUX");
        run_bounded(command, Duration::from_secs(5), 1024 * 1024).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let mut command = Command::new(TMUX);
        command.arg("-S").arg(&self.socket).arg("kill-server");
        if let Err(error) = run_bounded(command, Duration::from_secs(5), 1024 * 1024) {
            eprintln!("Could not stop isolated tmux fixture: {error}");
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn absent_isolated_server_lists_no_sessions() {
    let server = Server::new("absent");
    assert_eq!(server.tmux().list_sessions().unwrap(), vec![]);
}

#[test]
fn create_list_details_capture_rename_kill_on_isolated_server() {
    let server = Server::new("life");
    let tmux = server.tmux();
    let work = server.root.join("work dir");
    fs::create_dir(&work).unwrap();

    let created = tmux
        .create_session("cwx-alpha", work.to_str().unwrap())
        .unwrap();
    assert_eq!(created.name, "cwx-alpha");
    assert!(created.id.starts_with('$'));
    assert_eq!(created.windows, 1);
    assert_eq!(created.attached, 0);
    assert!(created.created > 1_700_000_000);

    let listed = tmux.list_sessions().unwrap();
    assert_eq!(listed, vec![created.clone()]);

    let details = tmux.session_details(&created.id).unwrap();
    assert_eq!(details.session, created);
    assert_eq!(details.windows.len(), 1);
    let pane = &details.windows[0].panes[0];
    assert!(pane.id.starts_with('%'));
    assert!(pane.active);
    assert!(pane.width > 0 && pane.height > 0);
    let canonical = fs::canonicalize(&work).unwrap();
    assert_eq!(fs::canonicalize(&pane.path).unwrap(), canonical);

    let marker = "cwelux-capture-marker-42";
    let out = server.raw(&[
        "send-keys",
        "-t",
        &pane.id,
        "-l",
        &format!("printf '%s\\n' {marker}"),
    ]);
    assert!(out.status.success());
    server.raw(&["send-keys", "-t", &pane.id, "Enter"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut captured = String::new();
    while Instant::now() < deadline {
        captured = tmux.capture_pane(&created.id, &pane.id).unwrap();
        if captured.lines().any(|l| l == marker) {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(
        captured.lines().any(|l| l == marker),
        "capture was {captured:?}"
    );
    assert!(captured.lines().count() <= 200);

    tmux.rename_session(&created.id, "cwx-beta").unwrap();
    let renamed = tmux.list_sessions().unwrap();
    assert_eq!(renamed[0].id, created.id, "IDs stay stable across rename");
    assert_eq!(renamed[0].name, "cwx-beta");

    // A name starting with '-' must be passed as a value, not parsed as a flag.
    tmux.rename_session(&created.id, "-dash").unwrap();
    assert_eq!(tmux.list_sessions().unwrap()[0].name, "-dash");

    assert!(
        tmux.create_session("-dash", work.to_str().unwrap())
            .is_err(),
        "duplicate name"
    );

    tmux.kill_session(&created.id).unwrap();
    assert!(tmux.session_details(&created.id).is_err());
}

#[test]
fn decodes_arbitrary_existing_names_and_paths() {
    let server = Server::new("odd");
    let tmux = server.tmux();
    let odd_dir = server.root.join("dir 'q' \"d\" $HOME ;#=% x\ny");
    fs::create_dir(&odd_dir).unwrap();
    // tmux itself vis-encodes backslashes in names, so the fixture avoids them.
    let odd_name = "odd name $x;#y 'q' %= \"d\"";
    let out = server.raw(&[
        "new-session",
        "-d",
        "-s",
        odd_name,
        "-c",
        odd_dir.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    server.raw(&["rename-window", "-t", odd_name, "win ;$ 'w'"]);

    let sessions = tmux.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name, odd_name);

    let details = tmux.session_details(&sessions[0].id).unwrap();
    assert_eq!(details.windows[0].name, "win ;$ 'w'");
    let pane_path = PathBuf::from(&details.windows[0].panes[0].path);
    assert_eq!(
        fs::canonicalize(pane_path).unwrap(),
        fs::canonicalize(&odd_dir).unwrap()
    );

    // Managed by ID even though the name fails new-name validation.
    tmux.kill_session(&sessions[0].id).unwrap();
}

#[test]
fn capture_rejects_pane_from_another_session() {
    let server = Server::new("member");
    let tmux = server.tmux();
    let a = tmux
        .create_session("cwx-a", server.root.to_str().unwrap())
        .unwrap();
    let b = tmux
        .create_session("cwx-b", server.root.to_str().unwrap())
        .unwrap();
    let b_pane = tmux.session_details(&b.id).unwrap().windows[0].panes[0]
        .id
        .clone();
    let err = tmux.capture_pane(&a.id, &b_pane).unwrap_err();
    assert!(err.contains("not in session"), "{err}");
}

#[test]
fn missing_session_is_an_error_not_empty() {
    let server = Server::new("gone");
    let tmux = server.tmux();
    tmux.create_session("cwx-keep", server.root.to_str().unwrap())
        .unwrap();
    assert!(tmux.session_details("$9999").is_err());
    assert!(tmux.kill_session("$9999").is_err());
    assert!(tmux.rename_session("$9999", "x").is_err());
}

#[test]
fn attach_argv_targets_exact_server_socket() {
    let server = Server::new("attach");
    let tmux = server.tmux();
    let s = tmux
        .create_session("cwx-att", server.root.to_str().unwrap())
        .unwrap();
    let argv = tmux.attach_argv(&s.id).unwrap();
    let real_socket = fs::canonicalize(&server.socket).unwrap();
    assert_eq!(argv[0], OsString::from(TMUX));
    assert_eq!(argv[1], OsString::from("-S"));
    assert_eq!(fs::canonicalize(&argv[2]).unwrap(), real_socket);
    assert_eq!(
        &argv[3..],
        &[
            OsString::from("attach-session"),
            "-t".into(),
            s.id.clone().into()
        ]
    );
    assert!(tmux.attach_argv("$9999").is_err());
}

// ---- Fake-binary fixtures at the public read boundary ----

fn fake_tmux(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("tmux");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn fixture_tmux(tag: &str, body: &str) -> Tmux {
    let dir = temp_dir(tag);
    let binary = fake_tmux(&dir, body);
    Tmux::new(config(binary, dir.join("unused.sock")))
}

#[test]
fn fixture_valid_output_is_parsed() {
    let tmux = fixture_tmux("valid", r"printf '%s\n' '\$3 fixture 2 1 1729740000;'");
    let sessions = tmux.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name, "fixture");
}

#[test]
fn fixture_missing_server_is_empty_success() {
    let tmux = fixture_tmux(
        "missing",
        "echo 'no server running on /tmp/x.sock' >&2; exit 1",
    );
    assert_eq!(tmux.list_sessions().unwrap(), vec![]);
}

#[test]
fn fixture_malformed_output_is_an_error() {
    let tmux = fixture_tmux(
        "malformed",
        "printf 'garbage without terminator\\n'; exit 0",
    );
    let err = tmux.list_sessions().unwrap_err();
    assert!(err.contains("unexpected tmux output"), "{err}");
}

#[test]
fn fixture_command_failure_is_an_error() {
    let tmux = fixture_tmux(
        "denied",
        "echo 'error connecting to /tmp/x.sock (Permission denied)' >&2; exit 1",
    );
    let err = tmux.list_sessions().unwrap_err();
    assert!(err.contains("Permission denied"), "{err}");
}

#[test]
fn fixture_hung_tmux_times_out() {
    let dir = temp_dir("hung");
    let binary = fake_tmux(&dir, "sleep 10");
    let mut cfg = config(binary, dir.join("unused.sock"));
    cfg.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let err = Tmux::new(cfg).list_sessions().unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn fixture_receives_direct_argv_and_deliberate_environment() {
    let dir = temp_dir("argv");
    let record = dir.join("record.txt");
    let body = format!(
        "{{ printf 'PATH=%s\\n' \"$PATH\"; printf 'TMUX=%s\\n' \"${{TMUX-unset}}\"; for a in \"$@\"; do printf '[%s]\\n' \"$a\"; done; }} > '{}'\nprintf '%s\\n' '\\$1 x 1 0 1;'",
        record.display()
    );
    let binary = fake_tmux(&dir, &body);
    let mut cfg = config(binary, dir.join("s p a c e.sock"));
    cfg.path_env = OsString::from("/deliberate/bin:/usr/bin");
    Tmux::new(cfg).list_sessions().unwrap();
    let text = fs::read_to_string(record).unwrap();
    assert!(text.contains("PATH=/deliberate/bin:/usr/bin\n"), "{text}");
    assert!(text.contains("TMUX=unset\n"), "{text}");
    assert!(
        text.contains(&format!(
            "[-S]\n[{}]\n",
            dir.join("s p a c e.sock").display()
        )),
        "{text}"
    );
    assert!(text.contains("[list-sessions]\n[-F]\n"), "{text}");
}
