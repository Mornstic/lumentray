use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::autostart;
use crate::hotkeys;

pub const SETTINGS_VERSION: u32 = 2;
const SAVE_DEBOUNCE: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

pub const DEFAULT_GLASS_OPACITY: u32 = 95;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MonitorSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_brightness: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    pub theme: ThemeChoice,
    pub glass_opacity: u32,
    pub launch_at_login: bool,
    pub restore_on_wake: bool,
    pub step: u32,
    pub hotkeys_enabled: bool,
    pub hotkey_increase: String,
    pub hotkey_decrease: String,
    pub hotkey_step: u32,
    pub monitors: HashMap<String, MonitorSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            theme: ThemeChoice::System,
            glass_opacity: DEFAULT_GLASS_OPACITY,
            launch_at_login: false,
            restore_on_wake: false,
            step: 5,
            hotkeys_enabled: false,
            hotkey_increase: hotkeys::DEFAULT_INCREASE.into(),
            hotkey_decrease: hotkeys::DEFAULT_DECREASE.into(),
            hotkey_step: 10,
            monitors: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub theme: ThemeChoice,
    pub glass_opacity: u32,
    pub launch_at_login: bool,
    pub restore_on_wake: bool,
    pub step: u32,
    pub hotkeys_enabled: bool,
    pub hotkey_increase: String,
    pub hotkey_decrease: String,
    pub hotkey_step: u32,
}

impl SettingsPatch {
    fn validate(&self) -> Result<(), String> {
        if !(1..=10).contains(&self.step) {
            return Err("调节步长需在 1% 到 10% 之间".into());
        }
        if !(1..=20).contains(&self.hotkey_step) {
            return Err("快捷键步长需在 1% 到 20% 之间".into());
        }
        if !(85..=100).contains(&self.glass_opacity) {
            return Err("玻璃不透明度需在 85% 到 100% 之间".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub theme: ThemeChoice,
    pub glass_opacity: u32,
    pub launch_at_login: bool,
    pub restore_on_wake: bool,
    pub step: u32,
    pub hotkeys_enabled: bool,
    pub hotkey_increase: String,
    pub hotkey_decrease: String,
    pub hotkey_step: u32,
    pub hotkey_status: Vec<hotkeys::HotkeyStatus>,
    pub monitor_count: usize,
    pub settings_path: String,
    pub version: u32,
    pub app_version: String,
}

struct Inner {
    settings: Settings,
    path: PathBuf,
    dirty: bool,
}

pub struct SettingsStore {
    inner: Arc<Mutex<Inner>>,
    kick: Sender<()>,
}

impl SettingsStore {
    pub fn load(app: &AppHandle) -> Self {
        let directory = config_directory(app);
        let path = directory.join("settings.json");

        let mut settings = match load_from(&path) {
            Ok(settings) => settings,
            Err(error) => {
                crate::trace(&format!("[settings] {error}"));
                Settings::default()
            }
        };
        settings.version = SETTINGS_VERSION;

        // The registry is the source of truth: the user may have moved the exe
        // or disabled startup from Task Manager since the last save.
        let enabled = autostart::is_enabled();
        let dirty = settings.launch_at_login != enabled;
        settings.launch_at_login = enabled;

        let inner = Arc::new(Mutex::new(Inner {
            settings,
            path,
            dirty,
        }));
        let (kick, receiver) = mpsc::channel();
        spawn_saver(Arc::clone(&inner), receiver);
        Self { inner, kick }
    }

    pub fn snapshot(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }

    pub fn monitor_count(&self) -> usize {
        self.inner.lock().unwrap().settings.monitors.len()
    }

    pub fn settings_path(&self) -> String {
        self.inner.lock().unwrap().path.display().to_string()
    }

    pub fn last_brightness(&self, id: &str) -> Option<u32> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .monitors
            .get(id)
            .and_then(|monitor| monitor.last_brightness)
    }

    pub fn record_last_brightness(&self, id: &str, percent: u32) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.settings.monitors.entry(id.to_string()).or_default();
        if entry.last_brightness == Some(percent) {
            return;
        }
        entry.last_brightness = Some(percent);
        inner.dirty = true;
        drop(inner);
        let _ = self.kick.send(());
    }

    pub fn apply_patch(&self, patch: &SettingsPatch) -> Result<Settings, String> {
        patch.validate()?;
        let (increase, decrease) =
            hotkeys::validate_pair(&patch.hotkey_increase, &patch.hotkey_decrease)?;

        let enabled = autostart::is_enabled();
        if patch.launch_at_login != enabled {
            autostart::set_enabled(patch.launch_at_login)?;
        }
        // Reflect what the registry actually says, not what was requested.
        let enabled = autostart::is_enabled();

        let mut inner = self.inner.lock().unwrap();
        inner.settings.theme = patch.theme;
        inner.settings.glass_opacity = patch.glass_opacity;
        inner.settings.restore_on_wake = patch.restore_on_wake;
        inner.settings.step = patch.step;
        inner.settings.hotkeys_enabled = patch.hotkeys_enabled;
        inner.settings.hotkey_increase = increase;
        inner.settings.hotkey_decrease = decrease;
        inner.settings.hotkey_step = patch.hotkey_step;
        inner.settings.launch_at_login = enabled;
        inner.dirty = true;
        save_locked(&mut inner)?;
        Ok(inner.settings.clone())
    }

    pub fn view(&self, app: &AppHandle) -> SettingsSnapshot {
        let settings = self.snapshot();
        SettingsSnapshot {
            theme: settings.theme,
            glass_opacity: settings.glass_opacity,
            launch_at_login: autostart::is_enabled(),
            restore_on_wake: settings.restore_on_wake,
            step: settings.step,
            hotkeys_enabled: settings.hotkeys_enabled,
            hotkey_increase: settings.hotkey_increase,
            hotkey_decrease: settings.hotkey_decrease,
            hotkey_step: settings.hotkey_step,
            hotkey_status: hotkeys::status(),
            monitor_count: self.monitor_count(),
            settings_path: self.settings_path(),
            version: settings.version,
            app_version: app.package_info().version.to_string(),
        }
    }

    pub fn save_now(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.dirty {
            save_locked(&mut inner)?;
        }
        Ok(())
    }
}

pub fn config_directory(app: &AppHandle) -> PathBuf {
    app.path()
        .config_dir()
        .map(|dir| dir.join("LumenTray"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn save_locked(inner: &mut Inner) -> Result<(), String> {
    save_to(&inner.path, &inner.settings)?;
    inner.dirty = false;
    Ok(())
}

fn spawn_saver(inner: Arc<Mutex<Inner>>, receiver: Receiver<()>) {
    let _ = thread::Builder::new()
        .name("lumentray-settings-save".into())
        .spawn(move || {
            loop {
                if receiver.recv().is_err() {
                    break;
                }
                // Coalesce bursts (slider drags) into one write.
                loop {
                    match receiver.recv_timeout(SAVE_DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                let result = {
                    let mut inner = inner.lock().unwrap();
                    if inner.dirty {
                        save_locked(&mut inner)
                    } else {
                        Ok(())
                    }
                };
                if let Err(error) = result {
                    crate::trace(&format!("[settings] save failed: {error}"));
                }
            }
        });
}

fn load_from(path: &Path) -> Result<Settings, String> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    match serde_json::from_str::<Settings>(&text) {
        Ok(mut settings) => {
            if !(1..=10).contains(&settings.step) {
                settings.step = 5;
            }
            if !(1..=20).contains(&settings.hotkey_step) {
                settings.hotkey_step = 10;
            }
            if !(85..=100).contains(&settings.glass_opacity) {
                settings.glass_opacity = DEFAULT_GLASS_OPACITY;
            }
            match hotkeys::validate_pair(&settings.hotkey_increase, &settings.hotkey_decrease) {
                Ok((increase, decrease)) => {
                    settings.hotkey_increase = increase;
                    settings.hotkey_decrease = decrease;
                }
                Err(_) => {
                    settings.hotkey_increase = hotkeys::DEFAULT_INCREASE.into();
                    settings.hotkey_decrease = hotkeys::DEFAULT_DECREASE.into();
                }
            }
            settings.version = SETTINGS_VERSION;
            Ok(settings)
        }
        Err(error) => {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            let corrupt = path.with_extension(format!("json.corrupt-{stamp}"));
            let _ = fs::rename(path, &corrupt);
            Err(format!(
                "解析 {} 失败（{error}），已移到 {}",
                path.display(),
                corrupt.display()
            ))
        }
    }
}

fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("无效的配置路径: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("创建 {} 失败: {error}", parent.display()))?;

    let json = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("序列化设置失败: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&temporary)
            .map_err(|error| format!("写入 {} 失败: {error}", temporary.display()))?;
        file.write_all(json.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("写入 {} 失败: {error}", temporary.display()))?;
    }

    if path.exists() {
        let _ = fs::copy(path, path.with_extension("json.bak"));
    }
    fs::rename(&temporary, path).map_err(|error| format!("替换 {} 失败: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lumentray-settings-test-{}-{name}.json",
            std::process::id()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("json.tmp"));
        let _ = fs::remove_file(path.with_extension("json.bak"));
        if let Some(parent) = path.parent()
            && let Ok(entries) = fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.contains("lumentray-settings-test") && name.contains("corrupt") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(settings.theme, ThemeChoice::Dark);
        assert_eq!(settings.glass_opacity, DEFAULT_GLASS_OPACITY);
        assert_eq!(settings.step, 5);
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(!settings.restore_on_wake);
        assert!(!settings.hotkeys_enabled);
        assert_eq!(settings.hotkey_increase, "Alt+F1");
        assert_eq!(settings.hotkey_decrease, "Alt+F2");
        assert_eq!(settings.hotkey_step, 10);
        assert!(settings.monitors.is_empty());
    }

    #[test]
    fn invalid_hotkeys_fall_back_on_load() {
        let path = temporary_path("hotkeys");
        cleanup(&path);
        fs::write(
            &path,
            r#"{"hotkeyIncrease":"F1","hotkeyDecrease":"Alt+F1","hotkeyStep":99}"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.hotkey_increase, "Alt+F1");
        assert_eq!(loaded.hotkey_decrease, "Alt+F2");
        assert_eq!(loaded.hotkey_step, 10);
        cleanup(&path);
    }

    #[test]
    fn round_trip_keeps_monitor_brightness_and_backs_up() {
        let path = temporary_path("roundtrip");
        cleanup(&path);
        let mut settings = Settings::default();
        settings.monitors.insert(
            "display#0".into(),
            MonitorSettings {
                last_brightness: Some(73),
            },
        );
        settings.theme = ThemeChoice::Light;
        settings.glass_opacity = 88;
        save_to(&path, &settings).unwrap();
        save_to(&path, &settings).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Light);
        assert_eq!(loaded.glass_opacity, 88);
        assert_eq!(
            loaded.monitors.get("display#0").unwrap().last_brightness,
            Some(73)
        );
        assert!(path.with_extension("json.bak").exists());
        cleanup(&path);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let path = temporary_path("corrupt");
        cleanup(&path);
        fs::write(&path, "{ not json").unwrap();
        let result = load_from(&path);
        assert!(result.is_err());
        assert!(!path.exists());
        cleanup(&path);
    }

    #[test]
    fn out_of_range_step_is_clamped_on_load() {
        let path = temporary_path("step");
        cleanup(&path);
        fs::write(&path, r#"{"step": 42}"#).unwrap();
        assert_eq!(load_from(&path).unwrap().step, 5);
        cleanup(&path);
    }

    #[test]
    fn patch_rejects_invalid_step() {
        let patch = SettingsPatch {
            theme: ThemeChoice::System,
            glass_opacity: DEFAULT_GLASS_OPACITY,
            launch_at_login: false,
            restore_on_wake: false,
            step: 0,
            hotkeys_enabled: false,
            hotkey_increase: "Alt+F1".into(),
            hotkey_decrease: "Alt+F2".into(),
            hotkey_step: 10,
        };
        assert!(patch.validate().is_err());

        let patch = SettingsPatch {
            step: 5,
            hotkey_step: 21,
            ..patch
        };
        assert!(patch.validate().is_err());
    }
}
