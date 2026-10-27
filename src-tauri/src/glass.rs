use tauri::{AppHandle, Manager, WebviewWindow};

use crate::settings::Theme;

/// Emitted on a window whose native material failed, so the UI can use its CSS fallback.
pub const GLASS_FLAG_EVENT: &str = "glass-unavailable";

/// Message logged and used to decide the CSS fallback. Pure so it is testable.
pub fn material_failure_message(window: &str, error: &str) -> String {
    format!("cwelux: glass material unavailable for {window} window: {error}")
}

#[cfg(target_os = "macos")]
pub fn apply_popover_material(window: &WebviewWindow) -> Result<(), String> {
    apply_material(
        window,
        "popover",
        window_vibrancy::NSVisualEffectMaterial::Popover,
    )
}

#[cfg(target_os = "macos")]
pub fn apply_settings_material(window: &WebviewWindow) -> Result<(), String> {
    apply_material(
        window,
        "settings",
        window_vibrancy::NSVisualEffectMaterial::Sidebar,
    )
}

#[cfg(not(target_os = "macos"))]
pub fn apply_popover_material(_window: &WebviewWindow) -> Result<(), String> {
    Err("unsupported platform".into())
}

#[cfg(not(target_os = "macos"))]
pub fn apply_settings_material(_window: &WebviewWindow) -> Result<(), String> {
    Err("unsupported platform".into())
}

/// `None` lets the material follow the system appearance.
pub fn native_theme(theme: Theme) -> Option<tauri::Theme> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    }
}

/// One call on the popover is enough: on macOS the theme is app-wide.
pub fn sync_theme(app: &AppHandle, theme: Theme) {
    let result = match app.get_webview_window(crate::WINDOW) {
        Some(window) => window
            .set_theme(native_theme(theme))
            .map_err(|e| e.to_string()),
        None => Err("popover window is missing".into()),
    };
    if let Err(e) = result {
        eprintln!("cwelux: could not set native theme: {e}");
    }
}

/// Must run on the main thread; window-vibrancy refuses otherwise.
#[cfg(target_os = "macos")]
fn apply_material(
    window: &WebviewWindow,
    name: &str,
    material: window_vibrancy::NSVisualEffectMaterial,
) -> Result<(), String> {
    use tauri::Emitter;

    window_vibrancy::apply_vibrancy(
        window,
        material,
        Some(window_vibrancy::NSVisualEffectState::Active),
        Some(22.0),
    )
    .map_err(|e| {
        let error = e.to_string();
        eprintln!("{}", material_failure_message(name, &error));
        if let Err(emit_error) = window.emit(GLASS_FLAG_EVENT, ()) {
            eprintln!("cwelux: could not signal missing glass: {emit_error}");
        }
        error
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_message_names_window_and_error() {
        assert_eq!(
            material_failure_message("settings", "not main thread"),
            "cwelux: glass material unavailable for settings window: not main thread"
        );
    }

    #[test]
    fn native_theme_follows_the_preference() {
        use crate::settings::Theme;
        assert_eq!(native_theme(Theme::System), None);
        assert_eq!(native_theme(Theme::Light), Some(tauri::Theme::Light));
        assert_eq!(native_theme(Theme::Dark), Some(tauri::Theme::Dark));
    }
}
