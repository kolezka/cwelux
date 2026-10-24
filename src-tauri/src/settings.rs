//! Persistent preferences and the launch-at-login toggle.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::tmux::expand_directory;

pub const FILE_NAME: &str = "preferences.json";
/// Points the preferences JSON at another directory. It never affects login items.
pub const CONFIG_DIR_ENV: &str = "CWELUX_CONFIG_DIR";
pub const REFRESH_INTERVALS: [u64; 3] = [2, 4, 10];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preferences {
    pub default_directory: String,
    pub refresh_interval_seconds: u64,
    pub theme: Theme,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            default_directory: "~".into(),
            refresh_interval_seconds: 4,
            theme: Theme::System,
        }
    }
}

impl Preferences {
    /// Shape checks that hold for any stored file, even if the directory was deleted later.
    fn validate_shape(&self) -> Result<(), String> {
        if !REFRESH_INTERVALS.contains(&self.refresh_interval_seconds) {
            return Err(format!(
                "Refresh interval must be 2, 4 or 10 seconds, not {}.",
                self.refresh_interval_seconds
            ));
        }
        let dir = &self.default_directory;
        if dir.is_empty() {
            return Err("Enter a default starting directory.".into());
        }
        if dir != "~" && !dir.starts_with("~/") && !Path::new(dir).is_absolute() {
            return Err("Default directory must be an absolute path or start with ~/.".into());
        }
        Ok(())
    }

    /// Full checks before saving: the directory must exist right now.
    pub fn validate_for_save(&self, home: Option<&Path>) -> Result<(), String> {
        self.validate_shape()?;
        expand_directory(&self.default_directory, home).map(drop)
    }
}

/// Resolves the preferences file: the override directory when set, else the app config dir.
pub fn preferences_path(
    override_dir: Option<OsString>,
    app_config_dir: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    match override_dir {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() {
                return Err(format!("{CONFIG_DIR_ENV} must be an absolute path."));
            }
            Ok(dir.join(FILE_NAME))
        }
        _ => Ok(app_config_dir()?.join(FILE_NAME)),
    }
}

/// Reads and writes one JSON file. Saves are serialized and atomic.
pub struct PreferencesStore {
    path: Result<PathBuf, String>,
    home: Option<PathBuf>,
    lock: Mutex<()>,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl PreferencesStore {
    pub fn new(path: Result<PathBuf, String>, home: Option<PathBuf>) -> Self {
        Self {
            path,
            home,
            lock: Mutex::new(()),
        }
    }

    fn path(&self) -> Result<&Path, String> {
        self.path
            .as_deref()
            .map_err(|e| format!("Settings location is unavailable: {e}"))
    }

    /// A missing file means defaults. Anything unreadable or malformed is an error.
    pub fn load(&self) -> Result<Preferences, String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<Preferences, String> {
        let path = self.path()?;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Preferences::default()),
            Err(e) => {
                return Err(format!(
                    "Could not read settings from {}: {e}",
                    path.display()
                ))
            }
        };
        let prefs: Preferences = serde_json::from_str(&text).map_err(|e| {
            format!(
                "Settings file {} is malformed ({e}). Fix or remove it.",
                path.display()
            )
        })?;
        prefs.validate_shape().map_err(|e| {
            format!(
                "Settings file {} is invalid: {e} Fix or remove it.",
                path.display()
            )
        })?;
        Ok(prefs)
    }

    /// Validates, refuses to replace an unreadable file, then writes via a temp file and rename.
    pub fn save(&self, prefs: &Preferences) -> Result<Preferences, String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        prefs.validate_for_save(self.home.as_deref())?;
        self.load_unlocked()?;
        let path = self.path()?;
        let dir = path
            .parent()
            .ok_or("Settings path has no parent directory.")?;
        fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        let mut body = serde_json::to_vec_pretty(prefs).map_err(|e| e.to_string())?;
        body.push(b'\n');
        let temp = dir.join(format!(
            ".{FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let written = write_synced(&temp, &body).and_then(|()| fs::rename(&temp, path));
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(format!(
                "Could not save settings to {}: {e}",
                path.display()
            ));
        }
        Ok(prefs.clone())
    }
}

fn write_synced(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(body)?;
    file.sync_all()
}

/// The OS login item, behind a trait so tests never touch the real one.
pub trait LoginItem: Send + Sync {
    fn is_enabled(&self) -> Result<bool, String>;
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
}

pub fn read_launch_at_login(item: &dyn LoginItem) -> Result<bool, String> {
    item.is_enabled()
        .map_err(|e| format!("Could not read the launch at login state: {e}"))
}

/// Applies the change, then reports what the OS says, never the requested value.
pub fn apply_launch_at_login(item: &dyn LoginItem, enabled: bool) -> Result<bool, String> {
    let verb = if enabled { "turn on" } else { "turn off" };
    item.set_enabled(enabled)
        .map_err(|e| format!("Could not {verb} launch at login: {e}"))?;
    let actual = read_launch_at_login(item)?;
    if actual != enabled {
        return Err(format!(
            "Launch at login is still {} after trying to {verb} it.",
            if actual { "on" } else { "off" }
        ));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cwelux-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_in(dir: &Path) -> PreferencesStore {
        PreferencesStore::new(
            Ok(dir.join("config").join(FILE_NAME)),
            Some(dir.to_path_buf()),
        )
    }

    fn prefs(dir: &str, seconds: u64, theme: Theme) -> Preferences {
        Preferences {
            default_directory: dir.into(),
            refresh_interval_seconds: seconds,
            theme,
        }
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(store_in(&dir).load().unwrap(), Preferences::default());
        assert_eq!(
            Preferences::default(),
            prefs("~", 4, Theme::System),
            "defaults are home, 4 seconds, system theme"
        );
    }

    #[test]
    fn round_trips_saved_preferences_in_camel_case() {
        let dir = temp_dir("roundtrip");
        fs::create_dir(dir.join("work")).unwrap();
        let store = store_in(&dir);
        let wanted = prefs("~/work", 10, Theme::Dark);
        assert_eq!(store.save(&wanted).unwrap(), wanted);
        assert_eq!(store_in(&dir).load().unwrap(), wanted);
        let text = fs::read_to_string(dir.join("config").join(FILE_NAME)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "defaultDirectory": "~/work",
                "refreshIntervalSeconds": 10,
                "theme": "dark"
            })
        );
        let leftovers: Vec<_> = fs::read_dir(dir.join("config"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn rejects_invalid_values_without_writing() {
        let dir = temp_dir("invalid");
        fs::write(dir.join("file"), "x").unwrap();
        let store = store_in(&dir);
        for bad in [
            prefs("~", 3, Theme::System),
            prefs("~", 0, Theme::System),
            prefs("", 4, Theme::System),
            prefs("relative/dir", 4, Theme::System),
            prefs("~/missing", 4, Theme::System),
            prefs("~/file", 4, Theme::System),
        ] {
            assert!(store.save(&bad).is_err(), "accepted {bad:?}");
        }
        assert!(
            !dir.join("config").exists(),
            "an invalid save wrote something"
        );
    }

    #[test]
    fn unknown_theme_or_interval_in_json_is_malformed() {
        for body in [
            r#"{"defaultDirectory":"~","refreshIntervalSeconds":4,"theme":"blue"}"#,
            r#"{"defaultDirectory":"~","refreshIntervalSeconds":5,"theme":"dark"}"#,
            r#"{"defaultDirectory":"~","refreshIntervalSeconds":"4","theme":"dark"}"#,
            r#"{"defaultDirectory":"","refreshIntervalSeconds":4,"theme":"dark"}"#,
            r#"{"defaultDirectory":"~","theme":"dark"}"#,
            r#"{"defaultDirectory":"~","refreshIntervalSeconds":4,"theme":"dark","extra":1}"#,
            "not json",
            "",
        ] {
            let dir = temp_dir("malformed-values");
            let store = store_in(&dir);
            fs::create_dir_all(dir.join("config")).unwrap();
            fs::write(dir.join("config").join(FILE_NAME), body).unwrap();
            let err = store.load().expect_err(body);
            assert!(err.contains("Fix or remove it"), "{body}: {err}");
        }
    }

    #[test]
    fn malformed_file_is_an_error_and_is_never_overwritten() {
        let dir = temp_dir("malformed");
        let store = store_in(&dir);
        let file = dir.join("config").join(FILE_NAME);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{\"theme\": ").unwrap();

        let err = store.load().unwrap_err();
        assert!(err.contains("malformed"), "{err}");
        assert!(store.save(&Preferences::default()).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"theme\": ");
    }

    #[test]
    fn unreadable_file_is_an_error_not_defaults() {
        let dir = temp_dir("unreadable");
        let file = dir.join("config").join(FILE_NAME);
        // A directory where the file should be cannot be read as text.
        fs::create_dir_all(&file).unwrap();
        let err = store_in(&dir).load().unwrap_err();
        assert!(err.contains("Could not read settings"), "{err}");
    }

    #[test]
    fn failed_write_keeps_the_previous_file() {
        let dir = temp_dir("failed-write");
        let store = store_in(&dir);
        let saved = prefs("~", 2, Theme::Light);
        store.save(&saved).unwrap();
        let config = dir.join("config");
        let mut perms = fs::metadata(&config).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        fs::set_permissions(&config, perms.clone()).unwrap();

        let result = store.save(&prefs("~", 10, Theme::Dark));

        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        fs::set_permissions(&config, perms).unwrap();
        assert!(result.unwrap_err().contains("Could not save settings"));
        assert_eq!(store.load().unwrap(), saved);
    }

    #[test]
    fn unavailable_location_is_an_error_on_both_paths() {
        let store = PreferencesStore::new(Err("no config dir".into()), None);
        assert!(store.load().unwrap_err().contains("no config dir"));
        assert!(store.save(&Preferences::default()).is_err());
    }

    #[test]
    fn concurrent_saves_leave_one_complete_file() {
        let dir = temp_dir("concurrent");
        let store = std::sync::Arc::new(store_in(&dir));
        let handles: Vec<_> = (0..12)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let theme = [Theme::System, Theme::Light, Theme::Dark][i % 3];
                    store.save(&prefs("~", REFRESH_INTERVALS[i % 3], theme))
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let loaded = store.load().unwrap();
        assert!(REFRESH_INTERVALS.contains(&loaded.refresh_interval_seconds));
        assert_eq!(fs::read_dir(dir.join("config")).unwrap().count(), 1);
    }

    #[test]
    fn override_dir_is_used_only_when_absolute() {
        let fallback = || Ok(PathBuf::from("/app/config"));
        assert_eq!(
            preferences_path(None, fallback).unwrap(),
            PathBuf::from("/app/config").join(FILE_NAME)
        );
        assert_eq!(
            preferences_path(Some("".into()), fallback).unwrap(),
            PathBuf::from("/app/config").join(FILE_NAME)
        );
        assert_eq!(
            preferences_path(Some("/tmp/x".into()), fallback).unwrap(),
            PathBuf::from("/tmp/x").join(FILE_NAME)
        );
        assert!(preferences_path(Some("rel".into()), fallback).is_err());
        assert!(preferences_path(None, || Err("none".into())).is_err());
    }

    struct FakeLogin {
        enabled: AtomicBool,
        fail_set: bool,
        fail_read: bool,
        ignore_set: bool,
    }

    impl FakeLogin {
        fn new(enabled: bool) -> Self {
            Self {
                enabled: AtomicBool::new(enabled),
                fail_set: false,
                fail_read: false,
                ignore_set: false,
            }
        }
    }

    impl LoginItem for FakeLogin {
        fn is_enabled(&self) -> Result<bool, String> {
            if self.fail_read {
                return Err("read denied".into());
            }
            Ok(self.enabled.load(Ordering::SeqCst))
        }
        fn set_enabled(&self, enabled: bool) -> Result<(), String> {
            if self.fail_set {
                return Err("write denied".into());
            }
            if !self.ignore_set {
                self.enabled.store(enabled, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    #[test]
    fn launch_at_login_reports_the_actual_state() {
        let item = FakeLogin::new(false);
        assert!(!read_launch_at_login(&item).unwrap());
        assert!(apply_launch_at_login(&item, true).unwrap());
        assert!(read_launch_at_login(&item).unwrap());
        assert!(!apply_launch_at_login(&item, false).unwrap());
    }

    #[test]
    fn launch_at_login_errors_never_claim_enabled() {
        let failing = FakeLogin {
            fail_set: true,
            ..FakeLogin::new(false)
        };
        let err = apply_launch_at_login(&failing, true).unwrap_err();
        assert!(err.contains("write denied"), "{err}");
        assert!(!failing.is_enabled().unwrap());

        let ignored = FakeLogin {
            ignore_set: true,
            ..FakeLogin::new(false)
        };
        let err = apply_launch_at_login(&ignored, true).unwrap_err();
        assert!(err.contains("still off"), "{err}");

        let unreadable = FakeLogin {
            fail_read: true,
            ..FakeLogin::new(false)
        };
        assert!(apply_launch_at_login(&unreadable, true).is_err());
        assert!(read_launch_at_login(&unreadable).is_err());
    }
}
