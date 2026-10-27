pub mod glass;
pub mod popover;
pub mod process;
pub mod settings;
pub mod terminal;
pub mod tmux;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Position, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt as _;

use popover::{HideReason, SettingsOpening};
use settings::{LoginItem, Preferences, PreferencesStore};
use terminal::Launcher;
use tmux::{Session, SessionDetails, Tmux, TmuxProvider};

const WINDOW: &str = "main";
const SETTINGS_WINDOW: &str = "settings";
const VISIBILITY_EVENT: &str = "popover-visibility";
const PREFERENCES_EVENT: &str = "preferences-changed";
/// A tray click right after a blur-hide is the same click that caused the blur.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

struct Backend {
    tmux: TmuxProvider,
    launcher: Arc<Launcher>,
}

impl Backend {
    /// Discovery is retried until it succeeds, so installing tmux and pressing retry works.
    fn tmux(&self) -> Result<Arc<Tmux>, String> {
        self.tmux.get()
    }
}

#[derive(Default)]
struct PopoverState {
    last_hidden: Mutex<Option<Instant>>,
    settings_opening: SettingsOpening,
}

/// The real login item, through the official autostart plugin.
struct AutostartLoginItem(AppHandle);

impl LoginItem for AutostartLoginItem {
    fn is_enabled(&self) -> Result<bool, String> {
        self.0.autolaunch().is_enabled().map_err(|e| e.to_string())
    }
    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        let manager = self.0.autolaunch();
        if enabled {
            manager.enable()
        } else {
            manager.disable()
        }
        .map_err(|e| e.to_string())
    }
}

/// Serializes login item changes so two toggles cannot interleave.
#[derive(Default)]
struct LoginItemLock(Mutex<()>);

/// Runs blocking tmux I/O off the async runtime threads.
async fn blocking<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
async fn list_sessions(backend: State<'_, Backend>) -> Result<Vec<Session>, String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.list_sessions()).await
}

#[tauri::command]
async fn session_details(
    backend: State<'_, Backend>,
    session_id: String,
) -> Result<SessionDetails, String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.session_details(&session_id)).await
}

#[tauri::command]
async fn capture_pane(
    backend: State<'_, Backend>,
    session_id: String,
    pane_id: String,
) -> Result<String, String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.capture_pane(&session_id, &pane_id)).await
}

#[tauri::command]
async fn create_session(
    backend: State<'_, Backend>,
    name: String,
    directory: String,
) -> Result<Session, String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.create_session(&name, &directory)).await
}

#[tauri::command]
async fn rename_session(
    backend: State<'_, Backend>,
    session_id: String,
    name: String,
) -> Result<(), String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.rename_session(&session_id, &name)).await
}

#[tauri::command]
async fn kill_session(backend: State<'_, Backend>, session_id: String) -> Result<(), String> {
    let tmux = backend.tmux()?;
    blocking(move || tmux.kill_session(&session_id)).await
}

#[tauri::command]
async fn open_session(backend: State<'_, Backend>, session_id: String) -> Result<(), String> {
    let tmux = backend.tmux()?;
    let launcher = backend.launcher.clone();
    blocking(move || {
        let argv = tmux.attach_argv(&session_id)?;
        launcher
            .launch(&argv)
            .map(drop)
            .map_err(|e| format!("{e} Copy the attach command and run it in a terminal instead."))
    })
    .await
}

#[tauri::command]
async fn attach_command(backend: State<'_, Backend>, session_id: String) -> Result<String, String> {
    let tmux = backend.tmux()?;
    blocking(move || terminal::command_line(&tmux.attach_argv(&session_id)?)).await
}

#[tauri::command]
fn hide_popover(app: AppHandle) -> Result<(), String> {
    match app.get_webview_window(WINDOW) {
        Some(window) => request_hide(&window, HideReason::Escape),
        None => Err("Popover window is missing.".into()),
    }
}

#[tauri::command]
async fn get_preferences(store: State<'_, PreferencesStore>) -> Result<Preferences, String> {
    store.load()
}

#[tauri::command]
async fn save_preferences(
    app: AppHandle,
    store: State<'_, PreferencesStore>,
    preferences: Preferences,
) -> Result<Preferences, String> {
    let saved = store.save(&preferences)?;
    // The file is already written, so a failed broadcast must not look like a failed save.
    if let Err(e) = app.emit(PREFERENCES_EVENT, &saved) {
        eprintln!("cwelux: could not broadcast new preferences: {e}");
    }
    Ok(saved)
}

// Async so window creation does not block the main thread it needs.
#[tauri::command]
async fn open_settings(app: AppHandle) -> Result<(), String> {
    show_settings(&app)
}

#[tauri::command]
async fn choose_directory(app: AppHandle) -> Result<Option<String>, String> {
    let mut dialog = app.dialog().file().set_title("Default starting directory");
    if let Some(parent) = app.get_webview_window(SETTINGS_WINDOW) {
        dialog = dialog.set_parent(&parent);
    }
    blocking(move || {
        let Some(picked) = dialog.blocking_pick_folder() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| e.to_string())?;
        path.to_str()
            .map(|p| Some(p.to_owned()))
            .ok_or_else(|| "The chosen folder path is not valid UTF-8.".into())
    })
    .await
}

#[tauri::command]
async fn get_launch_at_login(app: AppHandle) -> Result<bool, String> {
    blocking(move || settings::read_launch_at_login(&AutostartLoginItem(app))).await
}

#[tauri::command]
async fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, String> {
    blocking(move || {
        let lock = app.state::<LoginItemLock>();
        let _guard = lock.0.lock().unwrap_or_else(|e| e.into_inner());
        settings::apply_launch_at_login(&AutostartLoginItem(app.clone()), enabled)
    })
    .await
}

fn settings_open(app: &AppHandle) -> bool {
    app.get_webview_window(SETTINGS_WINDOW).is_some()
        || app.state::<PopoverState>().settings_opening.in_progress()
}

/// The one gate for focus loss, Escape, close requests and tray toggles.
fn request_hide(window: &WebviewWindow, reason: HideReason) -> Result<(), String> {
    if popover::hide_allowed(reason, settings_open(window.app_handle())) {
        hide(window)
    } else {
        Ok(())
    }
}

fn show_settings(app: &AppHandle) -> Result<(), String> {
    popover::open_or_focus(
        app.get_webview_window(SETTINGS_WINDOW),
        |window| {
            window.unminimize().map_err(|e| e.to_string())?;
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())
        },
        || {
            let state = app.state::<PopoverState>();
            let _opening = state.settings_opening.begin();
            // The popover floats above normal windows; drop that so it cannot cover settings.
            set_popover_floating(app, false);
            WebviewWindowBuilder::new(
                app,
                SETTINGS_WINDOW,
                WebviewUrl::App("settings.html".into()),
            )
            .title("Cwelux Settings")
            .inner_size(440.0, 520.0)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .center()
            .focused(true)
            .transparent(true)
            .build()
            .map(|window| {
                let glass_window = window.clone();
                if let Err(e) = window.run_on_main_thread(move || {
                    let _ = glass::apply_settings_material(&glass_window);
                }) {
                    eprintln!("cwelux: could not schedule settings glass: {e}");
                }
                let app = app.clone();
                window.on_window_event(move |event| {
                    if matches!(event, WindowEvent::Destroyed) {
                        // Runs after the window is unregistered, so the guard sees it closed.
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            set_popover_floating(&app, true);
                            hide_if_unfocused(&app);
                        });
                    }
                });
            })
            .map_err(|e| {
                set_popover_floating(app, true);
                format!("Could not open settings: {e}")
            })
        },
    )
    .map(drop)
}

fn set_popover_floating(app: &AppHandle, floating: bool) {
    if let Some(popover) = app.get_webview_window(WINDOW) {
        if let Err(e) = popover.set_always_on_top(floating) {
            eprintln!("cwelux: could not change popover level: {e}");
        }
    }
}

/// After settings close, a popover that lost focus meanwhile hides as it normally would.
fn hide_if_unfocused(app: &AppHandle) {
    let Some(window) = app.get_webview_window(WINDOW) else {
        return;
    };
    if window.is_visible().unwrap_or(false) && !window.is_focused().unwrap_or(false) {
        if let Err(e) = request_hide(&window, HideReason::FocusLost) {
            eprintln!("cwelux: could not hide popover: {e}");
        }
    }
}

fn hide(window: &WebviewWindow) -> Result<(), String> {
    if let Some(state) = window.try_state::<PopoverState>() {
        *state.last_hidden.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    window.hide().map_err(|e| e.to_string())?;
    window
        .emit(VISIBILITY_EVENT, false)
        .map_err(|e| e.to_string())
}

fn show_below(window: &WebviewWindow, icon_rect: &tauri::Rect) -> Result<(), String> {
    let guess_scale = window.scale_factor().map_err(|e| e.to_string())?;
    let guess = icon_rect.position.to_physical::<f64>(guess_scale);
    let monitor = window
        .monitor_from_point(guess.x, guess.y)
        .map_err(|e| e.to_string())?
        .or(window.primary_monitor().map_err(|e| e.to_string())?)
        .ok_or("No monitor found for the menu-bar icon.")?;
    let scale = monitor.scale_factor();
    let position = icon_rect.position.to_physical::<f64>(scale);
    let size = icon_rect.size.to_physical::<f64>(scale);
    let work = monitor.work_area();
    let window_size = window.outer_size().map_err(|e| e.to_string())?;
    let (x, y) = popover::popover_origin(
        popover::Rect {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
        f64::from(window_size.width),
        f64::from(window_size.height),
        popover::Rect {
            x: f64::from(work.position.x),
            y: f64::from(work.position.y),
            width: f64::from(work.size.width),
            height: f64::from(work.size.height),
        },
    );
    window
        .set_position(Position::Physical(PhysicalPosition::new(
            x.round() as i32,
            y.round() as i32,
        )))
        .map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    window
        .emit(VISIBILITY_EVENT, true)
        .map_err(|e| e.to_string())
}

fn toggle(app: &AppHandle, icon_rect: &tauri::Rect) {
    let Some(window) = app.get_webview_window(WINDOW) else {
        eprintln!("cwelux: popover window is missing");
        return;
    };
    let recently_hidden = app
        .state::<PopoverState>()
        .last_hidden
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|at| at.elapsed() < REOPEN_GUARD);
    let result = if window.is_visible().unwrap_or(false) {
        request_hide(&window, HideReason::TrayToggle)
    } else if recently_hidden {
        Ok(())
    } else {
        show_below(&window, icon_rect)
    };
    if let Err(e) = result {
        eprintln!("cwelux: could not toggle popover: {e}");
    }
}

pub fn run() {
    let backend = Backend {
        tmux: TmuxProvider::from_env(),
        launcher: Arc::new(Launcher::system()),
    };
    if let Err(e) = backend.tmux() {
        eprintln!("cwelux: tmux unavailable at startup: {e}");
    }

    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let config_override = std::env::var_os(settings::CONFIG_DIR_ENV);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .build(),
        )
        .manage(backend)
        .manage(PopoverState::default())
        .manage(LoginItemLock::default())
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            session_details,
            capture_pane,
            create_session,
            rename_session,
            kill_session,
            open_session,
            attach_command,
            hide_popover,
            get_preferences,
            save_preferences,
            open_settings,
            choose_directory,
            get_launch_at_login,
            set_launch_at_login
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let path = settings::preferences_path(config_override, || {
                app.path().app_config_dir().map_err(|e| e.to_string())
            });
            if let Err(e) = &path {
                eprintln!("cwelux: settings location unavailable: {e}");
            }
            app.manage(PreferencesStore::new(path, home));

            let window = app
                .get_webview_window(WINDOW)
                .ok_or("popover window missing from tauri.conf.json")?;
            // A failure is already logged and signalled to the UI.
            let _ = glass::apply_popover_material(&window);
            let events_window = window.clone();
            window.on_window_event(move |event| match event {
                WindowEvent::Focused(false) => {
                    if let Err(e) = request_hide(&events_window, HideReason::FocusLost) {
                        eprintln!("cwelux: could not hide popover: {e}");
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    if let Err(e) = request_hide(&events_window, HideReason::CloseRequested) {
                        eprintln!("cwelux: could not hide popover: {e}");
                    }
                }
                _ => {}
            });

            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Cwelux", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings, &quit])?;
            TrayIconBuilder::with_id("cwelux")
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("Cwelux")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "settings" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = show_settings(&app) {
                                eprintln!("cwelux: {e}");
                            }
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        toggle(tray.app_handle(), &rect);
                    }
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Cwelux");
}
