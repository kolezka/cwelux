# cwelux-agent

A small HTTP server for Linux. It lists the tmux sessions of the user it runs as, so Cwelux on
the Mac can show them. It is read-only: it accepts no commands and keeps no state.

It answers one request:

```
GET /v1/sessions
Authorization: Bearer <token>
```

```json
{
  "protocol": 1,
  "host": "local-arch",
  "capturedAt": 1761440000,
  "sessions": [
    { "id": "$3", "name": "work", "windows": 2, "attached": 1, "created": 1761400000 }
  ]
}
```

A wrong or missing token gets 401. If no tmux server is running, `sessions` is empty. If tmux
fails, the reply is 500 with `{"error": "..."}`.

There is no TLS. Run it on a private network such as Tailscale, which already encrypts the
traffic. The token keeps other peers on that network from reading the list.

## Limits

The agent serves at most 16 connections at once. Extra connections get 503 with
`{"error":"busy"}`. A client has 5 seconds to send its request head and the head may be at
most 8 KiB. A slow client is dropped without a reply, and a larger head gets 431. Each
connection carries one request and is closed after the reply. tmux runs for one request at a
time. A request that waits more than 5 seconds for its turn gets 503.

To keep everyone else off the port, bind only the Tailscale address with `--listen`, as
described under Configuration.

## Build

On the Linux host, with Rust 1.82 or newer:

```sh
cargo build --release
```

The binary is `target/release/cwelux-agent`.

To get a static binary that runs on any x86_64 Linux, build it with Docker from this
directory:

```sh
docker run --rm --platform linux/amd64 -v "$PWD":/work -w /work rust:1-alpine \
  sh -c 'apk add --no-cache musl-dev && cargo build --release --target x86_64-unknown-linux-musl'
```

The binary is `target/x86_64-unknown-linux-musl/release/cwelux-agent`. Keep
`--platform linux/amd64` on an Apple Silicon Mac, or Docker picks the arm64 image, which
has no x86_64 target installed.

## Install

Run these as the user who owns the tmux sessions, not as root.

Copy the binary:

```sh
mkdir -p ~/.local/bin
install -m 755 cwelux-agent ~/.local/bin/cwelux-agent
```

Create the token:

```sh
mkdir -p ~/.config/cwelux-agent
head -c 32 /dev/urandom | base64 | tr -d '/+=' | cut -c1-32 > ~/.config/cwelux-agent/token
chmod 600 ~/.config/cwelux-agent/token
```

Put the same token in `remote-hosts.json` on the Mac.

Install the systemd user unit:

```sh
mkdir -p ~/.config/systemd/user
cp cwelux-agent.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now cwelux-agent
```

A user unit stops when you log out. To keep it running after logout:

```sh
sudo loginctl enable-linger "$USER"
```

Logs: `journalctl --user -u cwelux-agent`. Each request is one line with the method, path,
status and time in ms. The path is cut to 120 characters, and anything other than printable
ASCII shows as `?`. Header values, including the token, are never logged.

## Test

```sh
curl -s -H "Authorization: Bearer $(cat ~/.config/cwelux-agent/token)" \
  http://127.0.0.1:7788/v1/sessions
```

From the Mac, use the host's Tailscale address instead of `127.0.0.1`.

## Configuration

Flags win over environment variables.

| Flag | Environment | Default |
| --- | --- | --- |
| `--listen <addr:port>` | `CWELUX_AGENT_LISTEN` | `0.0.0.0:7788` |
| `--token-file <path>` | `CWELUX_AGENT_TOKEN_FILE` | none, required |
| `--host-name <name>` | `CWELUX_AGENT_HOST_NAME` | `/etc/hostname`, then `hostname` |

The token is the first line of the token file, with surrounding whitespace trimmed. The agent refuses to start
if the file is missing, empty, or the token is shorter than 16 characters. Config errors exit
with code 2.

tmux is found through `CWELUX_TMUX_PATH` (an absolute path) or `PATH`, then `/usr/bin`,
`/usr/local/bin` and `/bin`. Set `CWELUX_TMUX_SOCKET` to an absolute socket path to read a
server other than the default one. Each tmux call times out after 5 seconds.

To bind only to the Tailscale address, pass it as `--listen`, for example
`--listen 100.64.0.5:7788`. Add it to `ExecStart` in the unit file.

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests run on macOS too. One test starts a private tmux server on a temp socket when
`/opt/homebrew/bin/tmux` exists, and skips otherwise.
