# Cwelux

Manage local tmux sessions from the macOS menu bar. Built with **Tauri 2, Rust, TypeScript and Bun**.

Cwelux handles the routine commands while your terminal stays available for interactive work. Closing the popover does not stop your sessions.

## Features

- **Find sessions:** search by name and see window counts and attached clients.
- **Inspect a session:** browse its windows and panes, current commands and working directories.
- **Preview output:** read the last 200 lines of a pane without attaching to it.
- **Manage sessions:** create a detached session, rename it, or end it after confirmation.
- **Return to the terminal:** open a session in the default terminal handler or copy its attach command.

Ending a session stops all processes in its panes. Cwelux asks for confirmation first.

## Requirements

- macOS. Other operating systems are not supported in this version.
- [tmux](https://github.com/tmux/tmux), installed separately. Cwelux does not bundle it.
- [Bun](https://bun.sh) and a current stable [Rust toolchain](https://rustup.rs).
- Xcode Command Line Tools for building the native app.

The initial app was tested with Bun 1.4.2, Rust 1.92.0 and tmux 3.8.

## Build and run

Install tmux if needed:

```sh
brew install tmux
```

Clone and build:

```sh
git clone https://github.com/kolezka/cwelux.git
cd cwelux
bun install --frozen-lockfile
bun run tauri build --bundles app
open src-tauri/target/release/bundle/macos/Cwelux.app
```

The app runs in the menu bar, not the Dock. Left-click its icon to toggle the popover. Right-click for Quit.

This produces a local app bundle. It is not a signed or notarized distribution release.

## Development

```sh
bun install --frozen-lockfile
bun run tauri dev
```

`bun run dev` starts only the frontend on port 1420. A plain browser cannot access the native tmux bridge and shows a desktop-app-required message instead of sample sessions.

## Default terminal

The open action passes a private `.command` launcher to macOS Launch Services. Cwelux does not hard-code Terminal or iTerm. The application associated with `.command` files must support executing them; if yours does not, use **Copy attach command** instead.

The attach command targets the same tmux server as the app. A successful handoff to macOS does not guarantee that another application has finished attaching.

## Environment overrides

Set these before launching the app:

| Variable | Purpose |
| --- | --- |
| `CWELUX_TMUX_PATH` | Absolute path to the tmux executable. Otherwise Cwelux searches PATH and standard Homebrew locations. |
| `CWELUX_TMUX_SOCKET` | Absolute socket path, passed to tmux as `-S`. Leave unset to use the normal local server. |

For example, to work with an explicitly selected test server:

```sh
CWELUX_TMUX_SOCKET=/absolute/path/to/test.sock bun run tauri dev
```

Invalid overrides produce an error rather than falling back to a different executable or server.

## Tests and checks

```sh
bun test
bun run check
bun run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --locked --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Build the frontend before running Rust checks from a fresh clone so the assets Tauri embeds are present.

Live tmux integration tests currently expect `/opt/homebrew/bin/tmux`. They use private temporary sockets rather than your default tmux server.

## Scope and limitations

- One local tmux server at a time. No SSH or embedded interactive terminal.
- Pane output is read-only and rendered as text.
- No application telemetry or cloud service.
- Other terminal handlers and mixed-DPI multi-monitor placement need further testing.
