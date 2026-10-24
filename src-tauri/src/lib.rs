pub mod popover;
pub mod process;
pub mod terminal;
pub mod tmux;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Position, State, WebviewWindow, WindowEvent,
};

use terminal::Launcher;
use tmux::{Session, SessionDetails, Tmux, TmuxProvider};

const WINDOW: &str = "main";
const VISIBILITY_EVENT: &str = "popover-visibility";
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
}

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
        Some(window) => hide(&window),
        None => Err("Popover window is missing.".into()),
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
        hide(&window)
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

    tauri::Builder::default()
        .manage(backend)
        .manage(PopoverState::default())
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            session_details,
            capture_pane,
            create_session,
            rename_session,
            kill_session,
            open_session,
            attach_command,
            hide_popover
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = app
                .get_webview_window(WINDOW)
                .ok_or("popover window missing from tauri.conf.json")?;
            let events_window = window.clone();
            window.on_window_event(move |event| match event {
                WindowEvent::Focused(false) => {
                    if let Err(e) = hide(&events_window) {
                        eprintln!("cwelux: could not hide popover: {e}");
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    if let Err(e) = hide(&events_window) {
                        eprintln!("cwelux: could not hide popover: {e}");
                    }
                }
                _ => {}
            });

            let quit = MenuItem::with_id(app, "quit", "Quit Cwelux", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::with_id("cwelux")
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("Cwelux")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    }
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
