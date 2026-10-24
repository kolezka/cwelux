//! tmux discovery, argv construction and strict output decoding.

use crate::process::{run_bounded, CommandOutput};
use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    pub id: String,
    pub index: u32,
    pub command: String,
    pub path: String,
    pub active: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxWindow {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetails {
    pub session: Session,
    pub windows: Vec<TmuxWindow>,
}

pub const PREVIEW_LINES: usize = 200;
/// Searched after PATH, in order, because GUI apps start with a minimal PATH.
pub const STANDARD_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/usr/bin",
    "/bin",
];
const SYSTEM_DIRS: &[&str] = &["/usr/sbin", "/sbin"];
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
pub const BINARY_ENV: &str = "CWELUX_TMUX_PATH";
pub const SOCKET_ENV: &str = "CWELUX_TMUX_SOCKET";

// Every field goes through `q:` so spaces and semicolons inside values come back
// backslash-escaped; an unescaped space separates fields and an unescaped `;` ends a record.
// The trailing `;` is written `\;` because tmux treats a bare trailing `;` as a command separator.
const SESSION_FORMAT: &str = "#{q:session_id} #{q:session_name} #{q:session_windows} \
     #{q:session_attached} #{q:session_created}\\;";
const SESSION_FIELDS: usize = 5;
const PANE_FORMAT: &str = "#{q:session_id} #{q:window_id} #{q:window_index} #{q:window_name} \
     #{q:window_active} #{q:pane_id} #{q:pane_index} #{q:pane_current_command} \
     #{q:pane_current_path} #{q:pane_active} #{q:pane_width} #{q:pane_height}\\;";
const PANE_FIELDS: usize = 12;
const SOCKET_FORMAT: &str = "#{q:session_id} #{q:socket_path}\\;";

#[derive(Debug, Clone)]
pub struct TmuxConfig {
    pub binary: PathBuf,
    /// `-S` socket. `None` means the user's default server.
    pub socket: Option<PathBuf>,
    /// `-f` config file, used by tests to keep the user's config out of private servers.
    pub config_file: Option<PathBuf>,
    pub path_env: OsString,
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
            path_env: spawn_path(&binary),
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

/// PATH for tmux and the shells it starts: the tmux directory first, then standard locations.
pub fn spawn_path(binary: &Path) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let candidates = binary
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .chain(STANDARD_DIRS.iter().chain(SYSTEM_DIRS).map(PathBuf::from));
    for dir in candidates {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

pub fn validate_new_name(name: &str) -> Result<(), String> {
    let valid_chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if name.is_empty() || name.len() > 64 || !valid_chars {
        return Err(
            "Session name must be 1 to 64 ASCII letters, digits, underscores or hyphens.".into(),
        );
    }
    Ok(())
}

fn valid_id(id: &str, prefix: char) -> bool {
    id.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.len() <= 10 && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

pub fn validate_session_id(id: &str) -> Result<(), String> {
    if valid_id(id, '$') {
        Ok(())
    } else {
        Err(format!("Invalid tmux session id: {id:?}"))
    }
}

pub fn validate_pane_id(id: &str) -> Result<(), String> {
    if valid_id(id, '%') {
        Ok(())
    } else {
        Err(format!("Invalid tmux pane id: {id:?}"))
    }
}

/// Expands `~` and `~/`, then requires an existing directory. Never creates anything.
pub fn expand_directory(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if input.is_empty() {
        return Err("Enter a starting directory.".into());
    }
    let path = if input == "~" || input.starts_with("~/") {
        let home = home.ok_or("Cannot expand ~ because the home directory is unknown.")?;
        match input.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => home.to_path_buf(),
        }
    } else if input.starts_with('~') {
        return Err("Only ~ and ~/ are supported in the starting directory.".into());
    } else {
        PathBuf::from(input)
    };
    if !path.is_absolute() {
        return Err("Starting directory must be an absolute path or start with ~/.".into());
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(path),
        Ok(_) => Err(format!("Not a directory: {}", path.display())),
        Err(e) => Err(format!(
            "Starting directory {} is not usable: {e}",
            path.display()
        )),
    }
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

fn flag(value: &str, what: &str) -> Result<bool, String> {
    match value {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(malformed(format!("{what} is not 0 or 1: {value:?}"))),
    }
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

/// Groups `list-panes -s` records into windows. Every record must belong to `session_id`.
pub fn parse_windows(output: &[u8], session_id: &str) -> Result<Vec<TmuxWindow>, String> {
    let mut windows: Vec<TmuxWindow> = Vec::new();
    for r in decode_records(output, PANE_FIELDS)? {
        if r[0] != session_id {
            return Err(malformed(format!(
                "pane listed for session {:?}, expected {session_id}",
                r[0]
            )));
        }
        let window_id = checked_id(&r[1], '@', "window id")?;
        let pane = Pane {
            id: checked_id(&r[5], '%', "pane id")?,
            index: number(&r[6], "pane index")?,
            command: r[7].clone(),
            path: r[8].clone(),
            active: flag(&r[9], "pane active")?,
            width: number(&r[10], "pane width")?,
            height: number(&r[11], "pane height")?,
        };
        match windows.iter_mut().find(|w| w.id == window_id) {
            Some(window) => window.panes.push(pane),
            None => windows.push(TmuxWindow {
                id: window_id,
                index: number(&r[2], "window index")?,
                name: r[3].clone(),
                active: flag(&r[4], "window active")?,
                panes: vec![pane],
            }),
        }
    }
    if windows.is_empty() {
        return Err(malformed(format!(
            "no panes listed for session {session_id}"
        )));
    }
    windows.sort_by_key(|w| w.index);
    for window in &mut windows {
        window.panes.sort_by_key(|p| p.index);
    }
    Ok(windows)
}

/// Last `limit` lines, ignoring the blank rows below the cursor on the visible screen.
pub fn last_lines(text: &str, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = end.saturating_sub(limit);
    lines[start..end].join("\n")
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

    pub fn config(&self) -> &TmuxConfig {
        &self.config
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
            .env("PATH", &self.config.path_env)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        // Finder launches often have no locale; without one tmux mangles UTF-8 names.
        if ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .all(|k| std::env::var_os(k).is_none_or(|v| v.is_empty()))
        {
            command.env("LANG", "en_US.UTF-8");
        }
        run_bounded(command, self.config.timeout, MAX_OUTPUT)
    }

    fn checked<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, String> {
        let output = self.run(args)?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(failure(args, &output))
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

    fn session(&self, session_id: &str) -> Result<Session, String> {
        let out = self.checked(&["display-message", "-p", "-t", session_id, SESSION_FORMAT])?;
        match parse_sessions(&out)?.as_slice() {
            [session] if session.id == session_id => Ok(session.clone()),
            other => Err(malformed(format!(
                "asked for session {session_id}, got {} records",
                other.len()
            ))),
        }
    }

    fn windows(&self, session_id: &str) -> Result<Vec<TmuxWindow>, String> {
        let out = self.checked(&["list-panes", "-s", "-t", session_id, "-F", PANE_FORMAT])?;
        parse_windows(&out, session_id)
    }

    pub fn session_details(&self, session_id: &str) -> Result<SessionDetails, String> {
        validate_session_id(session_id)?;
        let session = self.session(session_id)?;
        let windows = self.windows(session_id)?;
        Ok(SessionDetails { session, windows })
    }

    pub fn capture_pane(&self, session_id: &str, pane_id: &str) -> Result<String, String> {
        validate_session_id(session_id)?;
        validate_pane_id(pane_id)?;
        let member = self
            .windows(session_id)?
            .iter()
            .any(|w| w.panes.iter().any(|p| p.id == pane_id));
        if !member {
            return Err(format!("Pane {pane_id} is not in session {session_id}."));
        }
        let start = format!("-{PREVIEW_LINES}");
        let out = self.checked(&["capture-pane", "-p", "-t", pane_id, "-S", &start])?;
        Ok(last_lines(&String::from_utf8_lossy(&out), PREVIEW_LINES))
    }

    pub fn create_session(&self, name: &str, directory: &str) -> Result<Session, String> {
        validate_new_name(name)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let dir = expand_directory(directory, home.as_deref())?;
        let args: [&OsStr; 9] = [
            "new-session".as_ref(),
            "-d".as_ref(),
            "-P".as_ref(),
            "-F".as_ref(),
            SESSION_FORMAT.as_ref(),
            "-s".as_ref(),
            name.as_ref(),
            "-c".as_ref(),
            dir.as_os_str(),
        ];
        let out = self.checked(&args)?;
        match parse_sessions(&out)?.as_slice() {
            [session] => Ok(session.clone()),
            other => Err(malformed(format!(
                "new-session returned {} records",
                other.len()
            ))),
        }
    }

    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<(), String> {
        validate_session_id(session_id)?;
        validate_new_name(name)?;
        self.checked(&["rename-session", "-t", session_id, "--", name])
            .map(drop)
    }

    pub fn kill_session(&self, session_id: &str) -> Result<(), String> {
        validate_session_id(session_id)?;
        self.checked(&["kill-session", "-t", session_id]).map(drop)
    }

    /// argv (binary first) that attaches to the session on the exact server socket in use,
    /// so a terminal with a different TMUX_TMPDIR still reaches the same server.
    pub fn attach_argv(&self, session_id: &str) -> Result<Vec<OsString>, String> {
        validate_session_id(session_id)?;
        let out = self.checked(&["display-message", "-p", "-t", session_id, SOCKET_FORMAT])?;
        let records = decode_records(&out, 2)?;
        let [record] = records.as_slice() else {
            return Err(malformed("expected one socket record"));
        };
        if record[0] != session_id || !Path::new(&record[1]).is_absolute() {
            return Err(malformed(format!("bad socket record {record:?}")));
        }
        Ok(vec![
            self.config.binary.clone().into_os_string(),
            "-S".into(),
            record[1].clone().into(),
            "attach-session".into(),
            "-t".into(),
            session_id.into(),
        ])
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

type Resolver = Box<dyn Fn() -> Result<TmuxConfig, String> + Send + Sync>;

/// Lazily resolved tmux client. A failed discovery is retried on the next call
/// (tmux may have been installed since); a successful one is kept.
pub struct TmuxProvider {
    resolver: Resolver,
    cached: std::sync::Mutex<Option<Arc<Tmux>>>,
}

impl TmuxProvider {
    pub fn new(resolver: impl Fn() -> Result<TmuxConfig, String> + Send + Sync + 'static) -> Self {
        Self {
            resolver: Box::new(resolver),
            cached: std::sync::Mutex::new(None),
        }
    }

    pub fn from_env() -> Self {
        Self::new(TmuxConfig::from_env)
    }

    pub fn get(&self) -> Result<Arc<Tmux>, String> {
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tmux) = cached.as_ref() {
            return Ok(tmux.clone());
        }
        let tmux = Arc::new(Tmux::new((self.resolver)()?));
        *cached = Some(tmux.clone());
        Ok(tmux)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cwelux-unit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(socket: Option<&str>) -> TmuxConfig {
        TmuxConfig {
            binary: PathBuf::from("/opt/homebrew/bin/tmux"),
            socket: socket.map(PathBuf::from),
            config_file: None,
            path_env: OsString::from("/usr/bin:/bin"),
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
    }

    /// `window` is (id, index, name, active); `pane` is (id, index, active).
    fn pane_line(session: &str, window: (&str, u32, &str, u8), pane: (&str, u32, u8)) -> String {
        let (wid, windex, wname, wactive) = window;
        let (pid, pindex, pactive) = pane;
        format!("\\{session} \\{wid} {windex} {wname} {wactive} \\{pid} {pindex} zsh /tmp/a\\ dir {pactive} 80 24;\n")
    }

    #[test]
    fn groups_panes_into_sorted_windows() {
        let out = [
            pane_line("$1", ("@4", 1, "logs", 0), ("%7", 0, 1)),
            pane_line("$1", ("@3", 0, "dev", 1), ("%5", 1, 0)),
            pane_line("$1", ("@3", 0, "dev", 1), ("%4", 0, 1)),
        ]
        .concat();
        let windows = parse_windows(out.as_bytes(), "$1").unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, "@3");
        assert!(windows[0].active);
        assert_eq!(
            windows[0]
                .panes
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["%4", "%5"]
        );
        assert_eq!(windows[0].panes[0].path, "/tmp/a dir");
        assert_eq!(windows[1].name, "logs");
    }

    #[test]
    fn rejects_panes_from_another_session_or_empty_listing() {
        let foreign = pane_line("$2", ("@3", 0, "dev", 1), ("%4", 0, 1));
        assert!(parse_windows(foreign.as_bytes(), "$1").is_err());
        assert!(parse_windows(b"", "$1").is_err());
        let bad_bool = pane_line("$1", ("@3", 0, "dev", 2), ("%4", 0, 1));
        assert!(parse_windows(bad_bool.as_bytes(), "$1").is_err());
    }

    #[test]
    fn validates_new_names() {
        for ok in ["a", "dev_1", "my-session", &"x".repeat(64)] {
            assert!(validate_new_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a.b", "a:b", "zażółć", "$x", &"x".repeat(65)] {
            assert!(validate_new_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_ids() {
        assert!(validate_session_id("$0").is_ok());
        assert!(validate_session_id("$123").is_ok());
        for bad in ["", "$", "0", "$1a", "work", "$-1", "@1", "$1 "] {
            assert!(validate_session_id(bad).is_err(), "{bad}");
        }
        assert!(validate_pane_id("%5").is_ok());
        for bad in ["", "%", "5", "%5;", "$5"] {
            assert!(validate_pane_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn expands_home_and_requires_real_directory() {
        let home = temp_dir("home");
        fs::create_dir(home.join("proj")).unwrap();
        fs::write(home.join("file"), "x").unwrap();
        assert_eq!(expand_directory("~", Some(&home)).unwrap(), home);
        assert_eq!(
            expand_directory("~/proj", Some(&home)).unwrap(),
            home.join("proj")
        );
        assert_eq!(
            expand_directory(home.join("proj").to_str().unwrap(), None).unwrap(),
            home.join("proj")
        );
        assert!(expand_directory("~/missing", Some(&home)).is_err());
        assert!(expand_directory("~/file", Some(&home)).is_err());
        assert!(expand_directory("relative", Some(&home)).is_err());
        assert!(expand_directory("", Some(&home)).is_err());
        assert!(expand_directory("~other/x", Some(&home)).is_err());
        assert!(
            !home.join("missing").exists(),
            "must not create directories"
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
            Tmux::new(cfg).argv(&["kill-session", "-t", "$1"]),
            vec![
                OsString::from("-f"),
                "/dev/null".into(),
                "kill-session".into(),
                "-t".into(),
                "$1".into()
            ]
        );
    }

    #[test]
    fn invalid_input_is_rejected_before_running_tmux() {
        let mut cfg = config(None);
        cfg.binary = PathBuf::from("/nonexistent/tmux-should-not-run");
        let tmux = Tmux::new(cfg);
        assert!(tmux
            .rename_session("$1", "bad name")
            .unwrap_err()
            .contains("name"));
        assert!(tmux
            .kill_session("work")
            .unwrap_err()
            .contains("session id"));
        assert!(tmux
            .capture_pane("$1", "%x")
            .unwrap_err()
            .contains("pane id"));
        assert!(tmux
            .create_session("ok", "relative/dir")
            .unwrap_err()
            .contains("directory"));
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

    #[test]
    fn keeps_last_lines_without_trailing_blank_screen() {
        let text = (1..=250)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n\n\n";
        let out = last_lines(&text, PREVIEW_LINES);
        assert_eq!(out.lines().count(), 200);
        assert!(out.starts_with("51\n"));
        assert!(out.ends_with("250"));
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
    fn socket_override_must_be_absolute() {
        assert_eq!(socket_from_env(None).unwrap(), None);
        assert_eq!(
            socket_from_env(Some("/tmp/a.sock".into())).unwrap(),
            Some(PathBuf::from("/tmp/a.sock"))
        );
        assert!(socket_from_env(Some("rel.sock".into())).is_err());
        assert!(socket_from_env(Some("".into())).is_err());
    }

    #[test]
    fn provider_retries_failed_discovery_and_keeps_a_working_client() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let provider = TmuxProvider::new(move || {
            // First lookup: tmux not installed yet. Later lookups find it.
            match counter.fetch_add(1, Ordering::SeqCst) {
                0 => Err("tmux was not found".to_string()),
                _ => Ok(config(None)),
            }
        });
        let first_error = provider.get().err().expect("first lookup fails");
        assert!(first_error.starts_with("tmux was not found"));
        let first = provider
            .get()
            .expect("discovery must be retried after a failure");
        let second = provider.get().unwrap();
        assert!(Arc::ptr_eq(&first, &second), "a working client is reused");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "no lookups after success");
    }

    #[test]
    fn spawn_path_puts_tmux_dir_first_and_includes_system_dirs() {
        let path = spawn_path(Path::new("/custom/bin/tmux"));
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs[0], PathBuf::from("/custom/bin"));
        for required in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ] {
            assert!(dirs.contains(&PathBuf::from(required)), "{required}");
        }
    }
}
