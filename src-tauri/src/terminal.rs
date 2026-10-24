//! Opens a tmux attach command in the default `.command` handler via Launch Services.

use crate::process::run_bounded;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const OPEN: &str = "/usr/bin/open";
/// Launchers the handler never ran are removed on the next launch after this age.
pub const LAUNCHER_MAX_AGE: Duration = Duration::from_secs(600);
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// POSIX single-quote quoting. Control characters are refused rather than embedded.
pub fn shell_quote(arg: &OsStr) -> Result<String, String> {
    let bytes = arg.as_bytes();
    if bytes.iter().any(|&b| b < 0x20 || b == 0x7f) {
        return Err(format!(
            "Refusing to quote an argument with control characters: {arg:?}"
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| format!("Argument is not UTF-8: {arg:?}"))?;
    Ok(format!("'{}'", text.replace('\'', r"'\''")))
}

pub fn command_line(argv: &[OsString]) -> Result<String, String> {
    let quoted = argv
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(quoted.join(" "))
}

/// The launcher deletes itself first, so it is gone whether or not attach succeeds.
pub fn launcher_script(self_path: &Path, argv: &[OsString]) -> Result<String, String> {
    Ok(format!(
        "#!/bin/sh\n# Cwelux tmux launcher. Removes itself, then attaches.\n/bin/rm -f -- {}\nunset TMUX TMUX_PANE\nexec {}\n",
        shell_quote(self_path.as_os_str())?,
        command_line(argv)?
    ))
}

pub struct Launcher {
    dir: PathBuf,
    opener: PathBuf,
}

impl Launcher {
    pub fn new(dir: PathBuf, opener: PathBuf) -> Self {
        Self { dir, opener }
    }

    /// Default: private directory under the per-user temp dir, opened with /usr/bin/open.
    pub fn system() -> Self {
        Self::new(
            std::env::temp_dir().join("cwelux-launchers"),
            PathBuf::from(OPEN),
        )
    }

    /// Writes a private `.command` file and hands it to Launch Services. Returns its path.
    pub fn launch(&self, argv: &[OsString]) -> Result<PathBuf, String> {
        ensure_private_dir(&self.dir)?;
        sweep_stale(&self.dir);
        let (path, mut file) = create_unique(&self.dir)?;
        let written = launcher_script(&path, argv)
            .and_then(|script| file.write_all(script.as_bytes()).map_err(|e| e.to_string()));
        drop(file);
        if let Err(e) = written {
            let _ = fs::remove_file(&path);
            return Err(format!("Could not write the terminal launcher: {e}"));
        }
        let mut command = Command::new(&self.opener);
        command.arg(&path);
        let result = run_bounded(command, OPEN_TIMEOUT, 64 * 1024).and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(if stderr.is_empty() {
                    out.status.to_string()
                } else {
                    stderr
                })
            }
        });
        if let Err(e) = result {
            let _ = fs::remove_file(&path);
            return Err(format!(
                "Could not open the default terminal for .command files: {e}"
            ));
        }
        Ok(path)
    }
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?,
        Err(e) => return Err(format!("Could not inspect {}: {e}", dir.display())),
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(dir)
        .map_err(|e| format!("Could not inspect {}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!(
            "Launcher directory {} is not a real directory.",
            dir.display()
        ));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "Launcher directory {} has unsafe permissions or owner.",
            dir.display()
        ));
    }
    Ok(())
}

fn create_unique(dir: &Path) -> Result<(PathBuf, fs::File), String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for _ in 0..16 {
        let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "attach-{}-{nanos}-{seq}.command",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Could not create launcher in {}: {e}",
                    dir.display()
                ))
            }
        }
    }
    Err("Could not pick a unique launcher name.".into())
}

/// Best effort: a leftover file is harmless (it only attaches), so cleanup errors are not fatal.
fn sweep_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_launcher = path.extension().is_some_and(|e| e == "command")
            && entry.file_type().is_ok_and(|t| t.is_file());
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > LAUNCHER_MAX_AGE);
        if is_launcher && stale {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Instant;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cwelux-term-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_script(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    const TRICKY: &str = "it's a \"$HOME\" `x` \\ $(id) ;&|*";

    #[test]
    fn quotes_round_trip_through_sh() {
        let quoted = shell_quote(TRICKY.as_ref()).unwrap();
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s' {quoted}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), TRICKY);
        assert_eq!(shell_quote("".as_ref()).unwrap(), "''");
    }

    #[test]
    fn command_line_quotes_every_argument() {
        let argv: Vec<OsString> = vec![
            "/opt/homebrew/bin/tmux".into(),
            "-S".into(),
            "/tmp/a b".into(),
            "attach-session".into(),
            "-t".into(),
            "$1".into(),
        ];
        assert_eq!(
            command_line(&argv).unwrap(),
            "'/opt/homebrew/bin/tmux' '-S' '/tmp/a b' 'attach-session' '-t' '$1'"
        );
    }

    #[test]
    fn rejects_control_characters_in_launcher_arguments() {
        let argv: Vec<OsString> = vec!["/bin/tmux".into(), "-S".into(), "/tmp/a\nb".into()];
        assert!(command_line(&argv).is_err());
    }

    /// Fake opener runs the launcher the way a terminal would; fake tmux records argv.
    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = temp_dir(tag);
        let record = root.join("argv.txt");
        let tmux = root.join("fake tmux");
        write_script(
            &tmux,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n",
                record.display()
            ),
        );
        let opener = root.join("opener");
        write_script(
            &opener,
            &format!(
                "#!/bin/sh\nls -l \"$1\" > '{}/mode.txt'\nexec /bin/sh \"$1\"\n",
                root.display()
            ),
        );
        (root, tmux, opener, record)
    }

    #[test]
    fn launcher_preserves_argv_and_removes_itself() {
        let (root, tmux, opener, record) = fixture("launch");
        let socket = root.join(TRICKY).join("tmux.sock");
        let argv: Vec<OsString> = vec![
            tmux.into(),
            "-S".into(),
            socket.clone().into(),
            "attach-session".into(),
            "-t".into(),
            "$1".into(),
        ];
        let launcher = Launcher::new(root.join("launchers"), opener);
        let file = launcher.launch(&argv).unwrap();

        assert_eq!(file.extension().unwrap(), "command");
        assert!(!file.exists(), "launcher must delete itself");
        let recorded = fs::read_to_string(&record).unwrap();
        assert_eq!(
            recorded,
            format!("-S\n{}\nattach-session\n-t\n$1\n", socket.display())
        );
        let mode = fs::read_to_string(root.join("mode.txt")).unwrap();
        assert!(mode.starts_with("-rwx------"), "{mode}");
        let dir_mode = fs::metadata(root.join("launchers"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn failed_open_removes_launcher_and_reports_error() {
        let root = temp_dir("fail");
        let opener = root.join("opener");
        write_script(
            &opener,
            "#!/bin/sh\necho 'no application knows how to open' >&2\nexit 1\n",
        );
        let launcher = Launcher::new(root.join("launchers"), opener);
        let err = launcher
            .launch(&["/bin/tmux".into(), "attach-session".into()])
            .unwrap_err();
        assert!(err.contains("no application knows how to open"), "{err}");
        let left: Vec<_> = fs::read_dir(root.join("launchers")).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn refuses_insecure_launcher_directory() {
        let root = temp_dir("insecure");
        let dir = root.join("launchers");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        let launcher = Launcher::new(dir.clone(), PathBuf::from("/usr/bin/true"));
        assert!(launcher
            .launch(&["/bin/tmux".into()])
            .unwrap_err()
            .contains("permissions"));

        let link = root.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let launcher = Launcher::new(link, PathBuf::from("/usr/bin/true"));
        assert!(launcher.launch(&["/bin/tmux".into()]).is_err());
    }

    #[test]
    fn sweeps_stale_launchers_left_by_handlers_that_never_ran_them() {
        let root = temp_dir("sweep");
        let dir = root.join("launchers");
        let opener = root.join("opener");
        write_script(&opener, "#!/bin/sh\nexit 0\n");
        let launcher = Launcher::new(dir.clone(), opener);
        let first = launcher.launch(&["/bin/tmux".into()]).unwrap();
        assert!(first.exists(), "opener did not run it, so it stays for now");
        let old = std::time::SystemTime::now() - LAUNCHER_MAX_AGE - Duration::from_secs(5);
        let file = fs::File::options().write(true).open(&first).unwrap();
        file.set_modified(old).unwrap();
        drop(file);
        let started = Instant::now();
        launcher.launch(&["/bin/tmux".into()]).unwrap();
        assert!(!first.exists(), "stale launcher must be swept");
        assert!(started.elapsed() < Duration::from_secs(5));
        sleep(Duration::from_millis(1));
    }
}
