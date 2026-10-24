# Cwelux

A small macOS menu-bar app for local tmux sessions, built with Bun, TypeScript and Tauri 2.

## What it does

- Search sessions and see which have attached clients.
- Inspect windows, panes, working directories and current commands.
- Read the last 200 lines of a pane without attaching to it.
- Create a detached session, rename it or end it after confirmation.
- Open a session in the default terminal handler or copy its attach command.

Closing the popover leaves tmux sessions running. Ending a session stops the processes inside it. Cwelux does not embed a terminal or connect to remote hosts.

## Requirements

- macOS with Xcode Command Line Tools.
- [Bun](https://bun.sh), [Rust](https://rustup.rs) and [tmux](https://github.com/tmux/tmux).
- A terminal application associated with `.command` files for the open action.

Cwelux uses the installed tmux; it does not bundle or install it.

## Develop

```sh
bun install
bun run tauri dev
```

`bun run dev` starts only the frontend. It cannot access tmux without the Tauri backend and shows an unavailable message instead of fake sessions.

## Build

```sh
bun run tauri build --bundles app
```

The macOS app bundle is written under `src-tauri/target/release/bundle/macos/`. Local builds are not signed for distribution or notarized.

## Checks

```sh
bun test
bun run check
bun run build
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

The backend integration tests must use isolated tmux sockets, never the default server holding your normal sessions.

## Default terminal

The open action delegates a private `.command` launcher to macOS Launch Services. It does not force Terminal or iTerm. The application associated with `.command` files must support executing them. If your preferred terminal does not handle those files, copy the attach command instead. A successful handoff to macOS is not proof that another app has finished attaching.

## Environment overrides

Set these before launching the app, then restart it after changing them:

- `CWELUX_TMUX_PATH`: absolute path to a tmux executable. Otherwise Cwelux searches PATH and standard Homebrew locations.
- `CWELUX_TMUX_SOCKET`: absolute socket path, equivalent to tmux's `-S` option. Leave it unset for the normal local server. This is also how to test the GUI without touching your usual sessions.

```sh
CWELUX_TMUX_SOCKET=/absolute/path/to/test.sock bun run tauri dev
```

## Scope

This first version manages one local tmux server. Output previews are read-only, and terminal output is rendered as text. There is no telemetry, database, remote management or arbitrary shell command input.
