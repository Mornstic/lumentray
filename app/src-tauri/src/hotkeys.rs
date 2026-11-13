use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP, WM_HOTKEY};

use crate::{SettingsStore, cache};

pub const WM_RELOAD_HOTKEYS: u32 = WM_APP + 1;

const ID_INCREASE: i32 = 0x4C01;
const ID_DECREASE: i32 = 0x4C02;
const COALESCE: Duration = Duration::from_millis(80);
const CACHE_TTL: Duration = Duration::from_secs(6);

pub const DEFAULT_INCREASE: &str = "Alt+F1";
pub const DEFAULT_DECREASE: &str = "Alt+F2";

static STATUS: Mutex<Vec<HotkeyStatus>> = Mutex::new(Vec::new());
static SUSPENDED: AtomicBool = AtomicBool::new(false);
static EVENT_HWND: OnceLock<isize> = OnceLock::new();
static ACK: Mutex<Option<(u64, mpsc::Sender<()>)>> = Mutex::new(None);
static RELOAD_SEQ: AtomicU64 = AtomicU64::new(1);
static WORKER: OnceLock<mpsc::Sender<i32>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub action: String,
    pub accelerator: String,
    pub registered: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Accelerator {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
    pub display: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrightnessUpdate {
    id: String,
    percent: u32,
}

pub fn status() -> Vec<HotkeyStatus> {
    STATUS
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default()
}

pub fn set_window(hwnd: HWND) {
    let _ = EVENT_HWND.set(hwnd.0 as isize);
}

pub fn handle_message(app: &AppHandle, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    match msg {
        WM_HOTKEY => {
            on_hotkey(app, wparam.0 as i32);
            true
        }
        WM_RELOAD_HOTKEYS => {
            reload(app);
            signal_done(lparam.0 as u64);
            true
        }
        _ => false,
    }
}

pub fn init(app: &AppHandle) {
    reload(app);
}

pub fn request_reload(wait: Duration) -> bool {
    let Some(raw) = EVENT_HWND.get() else {
        return false;
    };
    let seq = RELOAD_SEQ.fetch_add(1, Ordering::Relaxed);
    let (receiver, sender) = if wait.is_zero() {
        (None, None)
    } else {
        let (sender, receiver) = mpsc::channel();
        (Some(receiver), Some(sender))
    };
    if let Some(sender) = sender
        && let Ok(mut ack) = ACK.lock()
    {
        *ack = Some((seq, sender));
    }
    let posted = unsafe {
        PostMessageW(
            Some(HWND(*raw as *mut core::ffi::c_void)),
            WM_RELOAD_HOTKEYS,
            WPARAM(0),
            LPARAM(seq as isize),
        )
    };
    if posted.is_err() {
        crate::trace("[hotkeys] reload message failed");
        return false;
    }
    match receiver {
        Some(receiver) => receiver.recv_timeout(wait).is_ok(),
        None => true,
    }
}

fn signal_done(seq: u64) {
    if let Ok(mut ack) = ACK.lock() {
        let matches = matches!(ack.as_ref(), Some((expected, _)) if *expected == seq);
        if matches && let Some((_, sender)) = ack.take() {
            let _ = sender.send(());
        }
    }
}

#[tauri::command]
pub fn set_hotkey_recording(recording: bool) {
    SUSPENDED.store(recording, Ordering::Relaxed);
    // 录制时先注销已注册的快捷键，否则系统会吞掉用户按下的组合键
    let _ = request_reload(Duration::ZERO);
}

fn reload(app: &AppHandle) {
    let Some(raw) = EVENT_HWND.get() else {
        return;
    };
    let hwnd = HWND(*raw as *mut core::ffi::c_void);
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), ID_INCREASE);
        let _ = UnregisterHotKey(Some(hwnd), ID_DECREASE);
    }

    if SUSPENDED.load(Ordering::Relaxed) {
        // 录制期间保持注销，让按键回到设置窗口；恢复时再按最新设置注册
        return;
    }

    let settings = app.state::<SettingsStore>().snapshot();
    let entries = [
        ("increase", settings.hotkey_increase.clone(), ID_INCREASE),
        ("decrease", settings.hotkey_decrease.clone(), ID_DECREASE),
    ];
    let mut statuses = Vec::with_capacity(entries.len());
    for (action, accelerator, id) in entries {
        let mut status = HotkeyStatus {
            action: action.into(),
            accelerator: accelerator.clone(),
            registered: false,
            error: None,
        };
        if !settings.hotkeys_enabled {
            statuses.push(status);
            continue;
        }
        match parse(&accelerator) {
            Ok(parsed) => {
                status.accelerator = parsed.display.clone();
                match unsafe { RegisterHotKey(Some(hwnd), id, parsed.modifiers, parsed.vk) } {
                    Ok(()) => {
                        status.registered = true;
                        crate::trace(&format!("[hotkeys] registered {action} {}", parsed.display));
                    }
                    Err(error) => {
                        let message = describe_register_error(&error);
                        crate::trace(&format!(
                            "[hotkeys] {action} {} failed: {message}",
                            parsed.display
                        ));
                        status.error = Some(message);
                    }
                }
            }
            Err(message) => {
                crate::trace(&format!(
                    "[hotkeys] invalid {action} {accelerator:?}: {message}"
                ));
                status.error = Some(message);
            }
        }
        statuses.push(status);
    }

    if let Ok(mut current) = STATUS.lock() {
        *current = statuses.clone();
    }
    let _ = app.emit("hotkeys-changed", statuses);
}

fn describe_register_error(error: &windows::core::Error) -> String {
    // HRESULT_FROM_WIN32(ERROR_HOTKEY_ALREADY_REGISTERED = 1409)
    if (error.code().0 as u32) & 0xFFFF == 1409 {
        "组合键已被其他程序占用".into()
    } else {
        format!("注册失败: {error}")
    }
}

fn on_hotkey(app: &AppHandle, id: i32) {
    if SUSPENDED.load(Ordering::Relaxed) {
        return;
    }
    let direction = match id {
        ID_INCREASE => 1,
        ID_DECREASE => -1,
        _ => return,
    };
    let sender = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        let worker_app = app.clone();
        let _ = thread::Builder::new()
            .name("lumentray-hotkey-adjust".into())
            .spawn(move || worker(worker_app, receiver));
        sender
    });
    let _ = sender.send(direction);
}

fn worker(app: AppHandle, receiver: mpsc::Receiver<i32>) {
    while let Ok(mut direction) = receiver.recv() {
        loop {
            // 按住快捷键时系统会连续投递 WM_HOTKEY，合并成一次调整。
            match receiver.recv_timeout(COALESCE) {
                Ok(next) => direction += next,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if direction != 0 {
            apply(&app, direction);
        }
    }
}

fn apply(app: &AppHandle, direction: i32) {
    let started = Instant::now();
    let step = app
        .state::<SettingsStore>()
        .snapshot()
        .hotkey_step
        .clamp(1, 20) as i32;
    let amount = direction * step;
    let values = current_brightnesses(app);
    if values.is_empty() {
        crate::trace("[hotkeys] no reachable monitor");
        return;
    }

    let total = values.len();
    let mut synced = 0usize;
    crate::note_user_write();
    for (id, current) in values {
        let target = (current as i32 + amount).clamp(0, 100) as u32;
        if target == current {
            synced += 1;
            continue;
        }
        let outcome = ddc_core::set_brightness(&id, target);
        if let Some(error) = &outcome.error {
            crate::trace(&format!(
                "[hotkeys] {id} -> {target}% failed: 0x{:08X} {}",
                error.code, error.context
            ));
            if let Some(actual) = outcome.brightness {
                cache::note(&id, actual.percent);
            }
            continue;
        }
        let percent = outcome
            .brightness
            .map(|brightness| brightness.percent)
            .unwrap_or(target);
        app.state::<SettingsStore>()
            .record_last_brightness(&id, percent);
        cache::note(&id, percent);
        cache::update_monitor(&id, percent);
        synced += 1;
        let _ = app.emit(
            "brightness-changed",
            vec![BrightnessUpdate {
                id: id.clone(),
                percent,
            }],
        );
    }
    crate::osd::show_after_hotkey(app, synced, total);
    let sign = if direction > 0 { "+" } else { "-" };
    crate::trace(&format!(
        "[perf] hotkey {sign}{step}% applied to {synced}/{total} monitors in {}ms",
        started.elapsed().as_millis()
    ));
}

fn current_brightnesses(app: &AppHandle) -> Vec<(String, u32)> {
    if let Some(map) = cache::brightnesses(CACHE_TTL) {
        return map.into_iter().collect();
    }
    let list = ddc_core::enumerate();
    let store = app.state::<SettingsStore>();
    let mut values = Vec::new();
    for monitor in &list.monitors {
        if let Some(brightness) = &monitor.brightness {
            store.record_last_brightness(&monitor.id, brightness.percent);
            values.push((monitor.id.clone(), brightness.percent));
        }
    }
    cache::remember(&list);
    if values.is_empty() {
        // 枚举暂时失败时退回每个显示器最后已知值，让写入去暴露真实错误。
        return cache::known_brightnesses().into_iter().collect();
    }
    values
}

pub fn note_brightness(id: &str, percent: u32) {
    cache::note(id, percent);
    cache::update_monitor(id, percent);
}

pub fn parse(input: &str) -> Result<Accelerator, String> {
    let mut control = false;
    let mut alt = false;
    let mut shift = false;
    let mut win = false;
    let mut key: Option<(u32, String)> = None;

    for token in input
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => control = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "win" | "meta" | "super" => win = true,
            _ => {
                if key.is_some() {
                    return Err("快捷键只能有一个主键".into());
                }
                key = Some(key_code(token)?);
            }
        }
    }

    let Some((vk, key_name)) = key else {
        return Err("快捷键缺少主键".into());
    };
    if !(control || alt || shift || win) {
        return Err("快捷键至少需要一个修饰键（Ctrl/Alt/Shift/Win）".into());
    }

    let mut modifiers = HOT_KEY_MODIFIERS(0);
    let mut parts = Vec::with_capacity(5);
    if control {
        modifiers |= MOD_CONTROL;
        parts.push("Ctrl");
    }
    if alt {
        modifiers |= MOD_ALT;
        parts.push("Alt");
    }
    if shift {
        modifiers |= MOD_SHIFT;
        parts.push("Shift");
    }
    if win {
        modifiers |= MOD_WIN;
        parts.push("Win");
    }
    parts.push(&key_name);
    Ok(Accelerator {
        modifiers,
        vk,
        display: parts.join("+"),
    })
}

pub fn validate_pair(increase: &str, decrease: &str) -> Result<(String, String), String> {
    let increase = parse(increase)?;
    let decrease = parse(decrease)?;
    if increase.display == decrease.display {
        return Err("调高和调低不能使用同一个快捷键".into());
    }
    Ok((increase.display, decrease.display))
}

fn key_code(token: &str) -> Result<(u32, String), String> {
    let upper = token.to_ascii_uppercase();
    if upper.len() == 1 {
        let ch = upper.chars().next().unwrap();
        if ch.is_ascii_alphabetic() || ch.is_ascii_digit() {
            return Ok((ch as u32, upper));
        }
    }
    if let Some(rest) = upper.strip_prefix('F')
        && let Ok(number) = rest.parse::<u32>()
        && (1..=24).contains(&number)
    {
        return Ok((0x70 + number - 1, format!("F{number}")));
    }
    let (vk, name) = match upper.as_str() {
        "UP" | "ARROWUP" => (0x26, "Up"),
        "DOWN" | "ARROWDOWN" => (0x28, "Down"),
        "LEFT" | "ARROWLEFT" => (0x25, "Left"),
        "RIGHT" | "ARROWRIGHT" => (0x27, "Right"),
        "HOME" => (0x24, "Home"),
        "END" => (0x23, "End"),
        "PAGEUP" | "PGUP" => (0x21, "PageUp"),
        "PAGEDOWN" | "PGDN" => (0x22, "PageDown"),
        "INSERT" | "INS" => (0x2D, "Insert"),
        "DELETE" | "DEL" => (0x2E, "Delete"),
        "SPACE" => (0x20, "Space"),
        "TAB" => (0x09, "Tab"),
        _ => return Err(format!("不支持的按键: {token}")),
    };
    Ok((vk, name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_accelerators() {
        let parsed = parse("alt+f1").unwrap();
        assert_eq!(parsed.display, "Alt+F1");
        assert_eq!(parsed.vk, 0x70);

        let parsed = parse("Shift+Ctrl+ArrowUp").unwrap();
        assert_eq!(parsed.display, "Ctrl+Shift+Up");

        let parsed = parse("win + space").unwrap();
        assert_eq!(parsed.display, "Win+Space");
        assert_eq!(parsed.vk, 0x20);
    }

    #[test]
    fn rejects_incomplete_or_duplicate_accelerators() {
        assert!(parse("F1").is_err());
        assert!(parse("Ctrl").is_err());
        assert!(parse("Ctrl+A+B").is_err());
        assert!(parse("Ctrl+UnknownKey").is_err());
        assert!(parse("Ctrl+F25").is_err());
    }

    #[test]
    fn validates_pair_uniqueness() {
        assert!(validate_pair("Alt+F1", "Alt+F2").is_ok());
        assert!(validate_pair("Alt+F1", "ALT+f1").is_err());
        assert!(validate_pair("Alt+F1", "bad").is_err());
    }
}
