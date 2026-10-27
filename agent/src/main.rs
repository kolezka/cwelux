use cwelux_agent::process::run_bounded;
use cwelux_agent::server::{Agent, AgentConfig, Limits};
use cwelux_agent::tmux::{self, TmuxConfig};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const DEFAULT_LISTEN: &str = "0.0.0.0:7788";
const MIN_TOKEN_CHARS: usize = 16;
const LISTEN_ENV: &str = "CWELUX_AGENT_LISTEN";
const TOKEN_FILE_ENV: &str = "CWELUX_AGENT_TOKEN_FILE";
const HOST_NAME_ENV: &str = "CWELUX_AGENT_HOST_NAME";

const USAGE: &str = "\
Usage: cwelux-agent --token-file <path> [--listen <addr:port>] [--host-name <name>]

Serves this machine's tmux sessions at GET /v1/sessions for Cwelux. Read-only.

Options:
  --token-file <path>   File whose first line is the bearer token (16+ characters).
                        Env: CWELUX_AGENT_TOKEN_FILE
  --listen <addr:port>  Address to listen on. Default 0.0.0.0:7788.
                        Env: CWELUX_AGENT_LISTEN
  --host-name <name>    Host name to report. Default: /etc/hostname, then `hostname`.
                        Env: CWELUX_AGENT_HOST_NAME
  -h, --help            Show this help.

tmux is found through CWELUX_TMUX_PATH or PATH. CWELUX_TMUX_SOCKET selects a server socket.
";

#[derive(Debug, Default, PartialEq)]
struct Flags {
    listen: Option<String>,
    token_file: Option<OsString>,
    host_name: Option<String>,
    help: bool,
}

#[derive(Debug, PartialEq)]
struct Settings {
    listen: String,
    token_file: PathBuf,
    host_name: Option<String>,
}

fn parse_flags(args: impl IntoIterator<Item = OsString>) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let text = arg
            .to_str()
            .ok_or_else(|| format!("argument is not valid UTF-8: {arg:?}"))?;
        if text == "-h" || text == "--help" {
            flags.help = true;
            continue;
        }
        let (name, inline) = match text.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(OsString::from(value))),
            _ => (text, None),
        };
        if !matches!(name, "--listen" | "--token-file" | "--host-name") {
            return Err(format!("unknown argument: {text}"));
        }
        let value = match inline.or_else(|| args.next()) {
            Some(v) if !v.is_empty() => v,
            _ => return Err(format!("{name} needs a value")),
        };
        let utf8 = || {
            value
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{name} value is not valid UTF-8"))
        };
        match name {
            "--listen" => flags.listen = Some(utf8()?),
            "--host-name" => flags.host_name = Some(utf8()?),
            _ => flags.token_file = Some(value),
        }
    }
    Ok(flags)
}

/// Flags win over env. An empty env var counts as unset.
fn settings(flags: Flags, env: impl Fn(&str) -> Option<OsString>) -> Result<Settings, String> {
    let env = |key: &str| env(key).filter(|v| !v.is_empty());
    let env_utf8 = |key: &str| -> Result<Option<String>, String> {
        env(key)
            .map(|v| {
                v.into_string()
                    .map_err(|_| format!("{key} is not valid UTF-8"))
            })
            .transpose()
    };
    let listen = match flags.listen {
        Some(v) => v,
        None => env_utf8(LISTEN_ENV)?.unwrap_or_else(|| DEFAULT_LISTEN.to_string()),
    };
    listen
        .parse::<std::net::SocketAddr>()
        .map_err(|_| format!("listen address must look like 0.0.0.0:7788, got {listen:?}"))?;
    let token_file = flags
        .token_file
        .or_else(|| env(TOKEN_FILE_ENV))
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("a token file is required: pass --token-file or set {TOKEN_FILE_ENV}")
        })?;
    let host_name = match flags.host_name {
        Some(v) => Some(v),
        None => env_utf8(HOST_NAME_ENV)?,
    };
    Ok(Settings {
        listen,
        token_file,
        host_name,
    })
}

/// The trimmed first line of the file. Error messages never include the token.
fn read_token(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read token file {}: {e}", path.display()))?;
    let token = text.lines().next().unwrap_or_default().trim();
    if token.is_empty() {
        return Err(format!("token file {} is empty", path.display()));
    }
    if token.chars().count() < MIN_TOKEN_CHARS {
        return Err(format!(
            "token in {} is shorter than {MIN_TOKEN_CHARS} characters",
            path.display()
        ));
    }
    Ok(token.to_string())
}

fn system_host_name() -> Result<String, String> {
    let from_file = std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !from_file.is_empty() {
        return Ok(from_file);
    }
    let from_command = run_bounded(Command::new("hostname"), Duration::from_secs(5), 64 * 1024)
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    if from_command.is_empty() {
        return Err(format!(
            "could not find the host name in /etc/hostname or from `hostname`; pass --host-name or set {HOST_NAME_ENV}"
        ));
    }
    Ok(from_command)
}

fn start() -> Result<Option<Agent>, (u8, String)> {
    let config_error = |e: String| (2, e);
    let flags = parse_flags(std::env::args_os().skip(1)).map_err(config_error)?;
    if flags.help {
        print!("{USAGE}");
        return Ok(None);
    }
    let settings = settings(flags, |k| std::env::var_os(k)).map_err(config_error)?;
    let token = read_token(&settings.token_file).map_err(config_error)?;
    let host = match settings.host_name {
        Some(name) => name.trim().to_string(),
        None => system_host_name().map_err(config_error)?,
    };
    if host.is_empty() {
        return Err((2, "host name is empty".into()));
    }
    // A bad socket setting is a config mistake, so catch it now instead of on every request.
    tmux::socket_from_env(std::env::var_os(tmux::SOCKET_ENV)).map_err(config_error)?;
    let config = AgentConfig {
        token,
        host,
        tmux: Box::new(TmuxConfig::from_env),
        limits: Limits::default(),
    };
    let agent = Agent::bind(&settings.listen, config).map_err(|e| (1, e))?;
    eprintln!("cwelux-agent listening on {}", agent.local_addr());
    Ok(Some(agent))
}

fn main() -> ExitCode {
    match start() {
        Ok(Some(agent)) => {
            agent.serve();
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("cwelux-agent: {message}");
            if code == 2 {
                eprintln!("Run cwelux-agent --help for usage.");
            }
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn parses_separate_and_inline_values() {
        let flags = parse_flags(args(&[
            "--listen",
            "127.0.0.1:1",
            "--token-file=/t",
            "--host-name",
            "box",
        ]))
        .unwrap();
        assert_eq!(
            flags,
            Flags {
                listen: Some("127.0.0.1:1".into()),
                token_file: Some("/t".into()),
                host_name: Some("box".into()),
                help: false,
            }
        );
        assert!(parse_flags(args(&["-h"])).unwrap().help);
        assert!(parse_flags(args(&["--help"])).unwrap().help);
    }

    #[test]
    fn rejects_unknown_and_missing_values() {
        assert!(parse_flags(args(&["--port", "1"])).is_err());
        assert!(parse_flags(args(&["extra"])).is_err());
        assert!(parse_flags(args(&["--listen"])).is_err());
        assert!(parse_flags(args(&["--listen="])).is_err());
        assert!(parse_flags(args(&["--token-file", ""])).is_err());
    }

    #[test]
    fn flags_win_over_env_and_defaults_apply() {
        let env = env_of(&[
            (LISTEN_ENV, "127.0.0.1:9"),
            (TOKEN_FILE_ENV, "/env-token"),
            (HOST_NAME_ENV, "env-host"),
        ]);
        let from_env = settings(Flags::default(), &env).unwrap();
        assert_eq!(
            from_env,
            Settings {
                listen: "127.0.0.1:9".into(),
                token_file: "/env-token".into(),
                host_name: Some("env-host".into()),
            }
        );
        let flags = Flags {
            listen: Some("127.0.0.1:8".into()),
            token_file: Some("/flag-token".into()),
            host_name: Some("flag-host".into()),
            help: false,
        };
        let from_flags = settings(flags, &env).unwrap();
        assert_eq!(from_flags.listen, "127.0.0.1:8");
        assert_eq!(from_flags.token_file, PathBuf::from("/flag-token"));
        assert_eq!(from_flags.host_name.as_deref(), Some("flag-host"));

        let defaults = settings(Flags::default(), env_of(&[(TOKEN_FILE_ENV, "/t")])).unwrap();
        assert_eq!(defaults.listen, DEFAULT_LISTEN);
        assert_eq!(defaults.host_name, None);
    }

    #[test]
    fn requires_token_file_and_valid_listen_address() {
        assert!(settings(Flags::default(), env_of(&[])).is_err());
        assert!(settings(Flags::default(), env_of(&[(TOKEN_FILE_ENV, "")])).is_err());
        let bad_listen = env_of(&[(TOKEN_FILE_ENV, "/t"), (LISTEN_ENV, "7788")]);
        assert!(settings(Flags::default(), bad_listen).is_err());
    }

    #[test]
    fn reads_trimmed_first_line_and_enforces_length() {
        let dir = std::env::temp_dir().join(format!("cwelux-agent-token-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");

        fs::write(&file, "  0123456789abcdef  \nsecond line\n").unwrap();
        assert_eq!(read_token(&file).unwrap(), "0123456789abcdef");

        fs::write(&file, "0123456789abcde\n").unwrap();
        let err = read_token(&file).unwrap_err();
        assert!(err.contains("shorter than 16"), "{err}");
        assert!(
            !err.contains("0123456789abcde"),
            "error must not echo the token"
        );

        fs::write(&file, "\n0123456789abcdef\n").unwrap();
        assert!(read_token(&file).unwrap_err().contains("empty"));

        assert!(read_token(&dir.join("missing")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
