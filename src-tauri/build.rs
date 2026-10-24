const COMMANDS: &[&str] = &[
    "list_sessions",
    "session_details",
    "capture_pane",
    "create_session",
    "rename_session",
    "kill_session",
    "open_session",
    "attach_command",
    "hide_popover",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
