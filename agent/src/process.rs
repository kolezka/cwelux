//! Subprocess runner with a hard timeout and bounded output capture.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Grace period for pipes to reach EOF after the child exits.
const PIPE_GRACE: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(5);

type ReadResult = Result<Vec<u8>, String>;

/// Spawn one child at a time so a concurrent spawn cannot inherit another command's pipes
/// (a real race on macOS, where the tests also run).
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Runs `command` with stdin closed, killing it after `timeout`. Each stream is capped at
/// `max_bytes`; exceeding the cap, a timeout, or a pipe held open by a grandchild is an error.
pub fn run_bounded(
    mut command: Command,
    timeout: Duration,
    max_bytes: usize,
) -> Result<CommandOutput, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = command.get_program().to_string_lossy().into_owned();
    let spawned = {
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        command.spawn()
    };
    let mut child = spawned.map_err(|e| format!("could not start {program}: {e}"))?;
    let stdout = spawn_reader(child.stdout.take(), max_bytes);
    let stderr = spawn_reader(child.stderr.take(), max_bytes);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill(&mut child);
                return Err(format!(
                    "{program} timed out after {} ms",
                    timeout.as_millis()
                ));
            }
            Ok(None) => thread::sleep(POLL),
            Err(e) => {
                kill(&mut child);
                return Err(format!("could not wait for {program}: {e}"));
            }
        }
    };

    let pipe_deadline = Instant::now() + PIPE_GRACE;
    let stdout = collect(&stdout, pipe_deadline, &program, "stdout", max_bytes)?;
    let stderr = collect(&stderr, pipe_deadline, &program, "stderr", max_bytes)?;
    Ok(CommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    max_bytes: usize,
) -> Receiver<ReadResult> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            let _ = tx.send(Ok(Vec::new()));
            return;
        };
        let mut data = Vec::new();
        let mut chunk = [0u8; 8192];
        let result = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(data),
                Ok(n) if data.len() + n > max_bytes => break Err("overflow".to_string()),
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e.to_string()),
            }
        };
        // Dropping the pipe here makes a still-writing child fail with EPIPE instead of blocking.
        let _ = tx.send(result);
    });
    rx
}

fn collect(
    rx: &Receiver<ReadResult>,
    deadline: Instant,
    program: &str,
    stream: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let wait = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(wait) {
        Ok(Ok(data)) => Ok(data),
        Ok(Err(e)) if e == "overflow" => Err(format!(
            "{program} produced too much output on {stream} (limit {max_bytes} bytes)"
        )),
        Ok(Err(e)) => Err(format!("could not read {program} {stream}: {e}")),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "{program} exited but its {stream} stayed open (a background process holds it)"
        )),
        Err(RecvTimeoutError::Disconnected) => {
            Err(format!("{program} {stream} reader stopped unexpectedly"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    #[test]
    fn captures_stdout_stderr_and_status() {
        let out = run_bounded(
            sh("printf 'out'; printf 'err' >&2; exit 3"),
            Duration::from_secs(5),
            1024,
        )
        .unwrap();
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn kills_child_after_timeout() {
        let started = Instant::now();
        let err = run_bounded(sh("sleep 10"), Duration::from_millis(200), 1024).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn rejects_output_over_limit_without_hanging() {
        let started = Instant::now();
        let err = run_bounded(sh("yes"), Duration::from_secs(5), 64 * 1024).unwrap_err();
        assert!(err.contains("too much output"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn large_stderr_does_not_deadlock_stdout() {
        let out = run_bounded(
            sh("head -c 300000 /dev/zero >&2; printf done"),
            Duration::from_secs(5),
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(out.stdout, b"done");
        assert_eq!(out.stderr.len(), 300000);
    }

    /// macOS std creates pipes with pipe() + fcntl(FD_CLOEXEC), so a spawn on another thread can
    /// inherit them. A daemon-like child (a tmux server) then holds another command's stdout open.
    #[test]
    fn concurrent_spawns_do_not_leak_pipes_into_background_processes() {
        let workers: Vec<_> = (0..12)
            .map(|i| {
                std::thread::spawn(move || {
                    let script = if i % 2 == 0 {
                        "sleep 2 </dev/null >/dev/null 2>&1 &"
                    } else {
                        "printf ok"
                    };
                    (0..80)
                        .filter_map(|_| run_bounded(sh(script), Duration::from_secs(5), 1024).err())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let errors: Vec<String> = workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect();
        assert!(
            errors.is_empty(),
            "{} of 960 runs failed, first: {:?}",
            errors.len(),
            errors.first()
        );
    }

    #[test]
    fn grandchild_holding_pipe_does_not_hang() {
        let started = Instant::now();
        let result = run_bounded(sh("sleep 10 & printf hi"), Duration::from_millis(500), 1024);
        assert!(result.is_err(), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
