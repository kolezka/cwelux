//! tmux discovery and strict session list decoding, copied from the Mac app's `tmux.rs`.

use crate::process::{run_bounded, CommandOutput};
use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub name: String,
    pub windows: u32,
    pub attached: u32,
    pub created: u64,
}

/// Searched after PATH, in order.
pub const STANDARD_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/bin"];
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
pub const BINARY_ENV: &str = "CWELUX_TMUX_PATH";
pub const SOCKET_ENV: &str = "CWELUX_TMUX_SOCKET";

// Every field goes through `q:` so spaces and semicolons inside values come back
// backslash-escaped; an unescaped space separates fields and an unescaped `;` ends a record.
// The trailing `;` is written `\;` because tmux treats a bare trailing `;` as a command separator.
pub const SESSION_FORMAT: &str = "#{q:session_id} #{q:session_name} #{q:session_windows} \
     #{q:session_attached} #{q:session_created}\\;";
const SESSION_FIELDS: usize = 5;

#[derive(Debug, Clone)]
pub struct TmuxConfig {
    pub binary: PathBuf,
    /// `-S` socket. `None` means the user's default server.
    pub socket: Option<PathBuf>,
    /// `-f` config file, used by tests to keep the user's config out of private servers.
    pub config_file: Option<PathBuf>,
    pub timeout: Duration,
}

impl TmuxConfig {
    pub fn from_env() -> Result<Self, String> {
        let binary = resolve_binary(
            std::env::var_os(BINARY_ENV),
            std::env::var_os("PATH"),
            STANDARD_DIRS,
        )?;
        let socket = socket_from_env(std::env::var_os(SOCKET_ENV))?;
        Ok(Self {
            binary,
            socket,
            config_file: None,
            timeout: DEFAULT_TIMEOUT,
        })
    }
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// An explicit override must be valid on its own; it never falls back to a search.
pub fn resolve_binary(
    override_path: Option<OsString>,
    path_var: Option<OsString>,
    fallback_dirs: &[&str],
) -> Result<PathBuf, String> {
    if let Some(value) = override_path {
        let path = PathBuf::from(&value);
        if !path.is_absolute() {
            return Err(format!(
                "{BINARY_ENV} must be an absolute path, got {:?}",
                value
            ));
        }
        if !is_executable_file(&path) {
            return Err(format!(
                "{BINARY_ENV} does not point to an executable file: {}",
                path.display()
            ));
        }
        return Ok(path);
    }
    let from_path = path_var
        .as_deref()
        .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .unwrap_or_default();
    from_path
        .into_iter()
        .filter(|dir| dir.is_absolute())
        .chain(fallback_dirs.iter().map(PathBuf::from))
        .map(|dir| dir.join("tmux"))
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| {
            format!(
                "tmux was not found in PATH or in {}. Install tmux or set {BINARY_ENV}.",
                fallback_dirs.join(", ")
            )
        })
}

pub fn socket_from_env(value: Option<OsString>) -> Result<Option<PathBuf>, String> {
    match value {
        None => Ok(None),
        Some(v) if Path::new(&v).is_absolute() => Ok(Some(PathBuf::from(v))),
        Some(v) => Err(format!(
            "{SOCKET_ENV} must be an absolute socket path, got {v:?}"
        )),
    }
}

fn valid_id(id: &str, prefix: char) -> bool {
    id.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.len() <= 10 && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

fn malformed(detail: impl std::fmt::Display) -> String {
    format!("unexpected tmux output: {detail}")
}

/// Strict decoder for `q:`-escaped records. Anything off-format is an error, never a partial result.
pub fn decode_records(output: &[u8], fields: usize) -> Result<Vec<Vec<String>>, String> {
    let text = std::str::from_utf8(output).map_err(|e| malformed(format!("not UTF-8 ({e})")))?;
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_record = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        in_record = true;
        match c {
            '\\' => field.push(chars.next().ok_or_else(|| malformed("dangling escape"))?),
            ' ' => record.push(std::mem::take(&mut field)),
            ';' => {
                record.push(std::mem::take(&mut field));
                if record.len() != fields {
                    return Err(malformed(format!(
                        "expected {fields} fields, got {}",
                        record.len()
                    )));
                }
                if chars.next() != Some('\n') {
                    return Err(malformed("record terminator not followed by newline"));
                }
                records.push(std::mem::take(&mut record));
                in_record = false;
            }
            other => field.push(other),
        }
    }
    if in_record {
        return Err(malformed("unterminated record"));
    }
    Ok(records)
}

fn number<T: std::str::FromStr>(value: &str, what: &str) -> Result<T, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed(format!("{what} is not a number: {value:?}")));
    }
    value
        .parse()
        .map_err(|_| malformed(format!("{what} is out of range: {value:?}")))
}

fn checked_id(value: &str, prefix: char, what: &str) -> Result<String, String> {
    if valid_id(value, prefix) {
        Ok(value.to_string())
    } else {
        Err(malformed(format!("bad {what} {value:?}")))
    }
}

pub fn parse_sessions(output: &[u8]) -> Result<Vec<Session>, String> {
    decode_records(output, SESSION_FIELDS)?
        .into_iter()
        .map(|r| {
            Ok(Session {
                id: checked_id(&r[0], '$', "session id")?,
                name: r[1].clone(),
                windows: number(&r[2], "window count")?,
                attached: number(&r[3], "attached count")?,
                created: number(&r[4], "creation time")?,
            })
        })
        .collect()
}

/// True only when nothing is listening on the socket. Permission and other errors are real failures.
pub fn is_missing_server(stderr: &str) -> bool {
    let line = stderr.trim();
    line.starts_with("no server running on ")
        || (line.starts_with("error connecting to ")
            && (line.ends_with("(No such file or directory)")
                || line.ends_with("(Connection refused)")))
}

pub struct Tmux {
    config: TmuxConfig,
}

impl Tmux {
    pub fn new(config: TmuxConfig) -> Self {
        Self { config }
    }

    /// Full argv after the binary, including server selection flags.
    pub fn argv<S: AsRef<OsStr>>(&self, args: &[S]) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(args.len() + 4);
        if let Some(socket) = &self.config.socket {
            argv.push("-S".into());
            argv.push(socket.clone().into_os_string());
        }
        if let Some(file) = &self.config.config_file {
            argv.push("-f".into());
            argv.push(file.clone().into_os_string());
        }
        argv.extend(args.iter().map(|a| a.as_ref().to_os_string()));
        argv
    }

    fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<CommandOutput, String> {
        let mut command = Command::new(&self.config.binary);
        command
            .args(self.argv(args))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        // Service managers often start us without a locale; without one tmux mangles UTF-8 names.
        if ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .all(|k| std::env::var_os(k).is_none_or(|v| v.is_empty()))
        {
            command.env("LANG", "C.UTF-8");
        }
        run_bounded(command, self.config.timeout, MAX_OUTPUT)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>, String> {
        let args = ["list-sessions", "-F", SESSION_FORMAT];
        let output = self.run(&args)?;
        if !output.status.success() {
            if is_missing_server(&String::from_utf8_lossy(&output.stderr)) {
                return Ok(Vec::new());
            }
            return Err(failure(&args, &output));
        }
        parse_sessions(&output.stdout)
    }
}

fn failure<S: AsRef<OsStr>>(args: &[S], output: &CommandOutput) -> String {
    let subcommand = args
        .first()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .unwrap_or_default();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("tmux {subcommand} failed ({})", output.status)
    } else {
        format!("tmux {subcommand} failed: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cwelux-agent-unit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(socket: Option<&str>) -> TmuxConfig {
        TmuxConfig {
            binary: PathBuf::from("/usr/bin/tmux"),
            socket: socket.map(PathBuf::from),
            config_file: None,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn decodes_escaped_fields_with_separators_inside_values() {
        let out = b"\\$1 a\\ b\\;c\\\\d 2;\n\\$2  0;\n";
        let records = decode_records(out, 3).unwrap();
        assert_eq!(
            records,
            vec![
                vec!["$1".to_string(), "a b;c\\d".into(), "2".into()],
                vec!["$2".to_string(), "".into(), "0".into()],
            ]
        );
    }

    #[test]
    fn decodes_raw_newline_inside_value() {
        let records = decode_records(b"a\nb c;\n", 2).unwrap();
        assert_eq!(records, vec![vec!["a\nb".to_string(), "c".into()]]);
    }

    #[test]
    fn empty_output_is_zero_records() {
        assert_eq!(decode_records(b"", 3).unwrap(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn rejects_malformed_records() {
        for bad in [
            &b"$1 a;\n"[..],        // too few fields
            b"$1 a b c;\n",         // too many fields
            b"$1 a b\n",            // unterminated record
            b"$1 a b;",             // missing newline after terminator
            b"$1 a b;\ngarbage",    // trailing junk
            b"$1 a b\\",            // dangling escape
            b"$1 \xff b;\n",        // invalid UTF-8
            b"no server running\n", // human text on stdout
        ] {
            assert!(
                decode_records(bad, 3).is_err(),
                "accepted {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn parses_sessions() {
        let out = b"\\$0 work\\ \\$HOME 3 1 1729740000;\n\\$12 x 1 0 1729740001;\n";
        let sessions = parse_sessions(out).unwrap();
        assert_eq!(
            sessions[0],
            Session {
                id: "$0".into(),
                name: "work $HOME".into(),
                windows: 3,
                attached: 1,
                created: 1729740000
            }
        );
        assert_eq!(sessions[1].id, "$12");
    }

    #[test]
    fn rejects_bad_session_values() {
        assert!(
            parse_sessions(b"1 work 3 1 1729740000;\n").is_err(),
            "id without $"
        );
        assert!(
            parse_sessions(b"\\$1 work x 1 1729740000;\n").is_err(),
            "non-numeric windows"
        );
        assert!(
            parse_sessions(b"\\$1 work 3 -1 1729740000;\n").is_err(),
            "negative attached"
        );
        assert!(
            parse_sessions(b"\\$12345678901 work 3 1 1729740000;\n").is_err(),
            "id longer than 10 digits"
        );
    }

    #[test]
    fn session_serializes_camel_case() {
        let session = Session {
            id: "$3".into(),
            name: "work".into(),
            windows: 2,
            attached: 1,
            created: 1761400000,
        };
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            serde_json::json!({
                "id": "$3", "name": "work", "windows": 2, "attached": 1, "created": 1761400000u64
            })
        );
    }

    #[test]
    fn argv_selects_socket_and_config() {
        let tmux = Tmux::new(config(Some("/tmp/x y/s.sock")));
        assert_eq!(
            tmux.argv(&["list-sessions"]),
            vec![
                OsString::from("-S"),
                "/tmp/x y/s.sock".into(),
                "list-sessions".into()
            ]
        );
        let mut cfg = config(None);
        cfg.config_file = Some(PathBuf::from("/dev/null"));
        assert_eq!(
            Tmux::new(cfg).argv(&["list-sessions"]),
            vec![
                OsString::from("-f"),
                "/dev/null".into(),
                "list-sessions".into()
            ]
        );
    }

    #[test]
    fn missing_server_messages() {
        assert!(is_missing_server(
            "no server running on /private/tmp/tmux-501/default\n"
        ));
        assert!(is_missing_server(
            "error connecting to /tmp/tmux-501/default (No such file or directory)\n"
        ));
        assert!(is_missing_server(
            "error connecting to /tmp/x.sock (Connection refused)\n"
        ));
        assert!(!is_missing_server(
            "error connecting to /tmp/x.sock (Permission denied)\n"
        ));
        assert!(!is_missing_server("can't find session: $3\n"));
        assert!(!is_missing_server(""));
    }

    fn write_exe(path: &Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn resolves_binary_from_override_path_and_fallbacks() {
        let dir = temp_dir("resolve");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("tmux"), "not executable").unwrap();
        write_exe(&b.join("tmux"));

        let path_var = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(
            resolve_binary(None, Some(path_var), &[]).unwrap(),
            b.join("tmux")
        );

        let fallback = b.to_str().unwrap();
        assert_eq!(
            resolve_binary(None, None, &[fallback]).unwrap(),
            b.join("tmux")
        );
        assert!(
            resolve_binary(None, Some(OsString::from(&a)), &[]).is_err(),
            "non-executable must be skipped"
        );

        assert_eq!(
            resolve_binary(Some(b.join("tmux").into()), None, &[]).unwrap(),
            b.join("tmux")
        );
        assert!(
            resolve_binary(Some("tmux".into()), None, &[fallback]).is_err(),
            "relative override"
        );
        assert!(
            resolve_binary(Some(a.join("tmux").into()), None, &[fallback]).is_err(),
            "invalid override must not fall back"
        );
    }

    #[test]
    fn linux_fallback_dirs_are_searched() {
        for dir in ["/usr/bin", "/usr/local/bin", "/bin"] {
            assert!(STANDARD_DIRS.contains(&dir), "{dir}");
        }
    }

    #[test]
    fn socket_override_must_be_absolute() {
        assert_eq!(socket_from_env(None).unwrap(), None);
        assert_eq!(
            socket_from_env(Some("/tmp/a.sock".into())).unwrap(),
            Some(PathBuf::from("/tmp/a.sock"))
        );
        assert!(socket_from_env(Some("rel.sock".into())).is_err());
        assert!(socket_from_env(Some("".into())).is_err());
    }
}
