use serde::Serialize;
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

use crate::hotkeys::HotkeyStatus;
use crate::settings::SettingsStore;

// CF_UNICODETEXT；避免为此启用整个 Win32_System_Ole feature。
const CF_UNICODETEXT: u32 = 13;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsInfo {
    pub app_version: String,
    pub exe_path: String,
    pub settings_path: String,
    pub log_path: String,
    pub hotkeys: Vec<HotkeyStatus>,
    pub monitor_count: usize,
    pub log_tail: Vec<String>,
}

#[tauri::command]
pub fn get_diagnostics(app: AppHandle) -> DiagnosticsInfo {
    let store = app.state::<SettingsStore>();
    DiagnosticsInfo {
        app_version: app.package_info().version.to_string(),
        exe_path: std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        settings_path: store.settings_path(),
        log_path: crate::logging::path()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        hotkeys: crate::hotkeys::status(),
        monitor_count: store.monitor_count(),
        log_tail: crate::logging::tail(120),
    }
}

#[tauri::command]
pub fn read_log_tail(lines: Option<usize>) -> Vec<String> {
    crate::logging::tail(lines.unwrap_or(150))
}

#[tauri::command]
pub fn open_log_folder() -> Result<(), String> {
    let path = crate::logging::path().ok_or_else(|| "日志尚未初始化".to_string())?;
    let directory = path.parent().map(|path| path.to_path_buf()).unwrap_or(path);
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("打开日志目录失败: {error}"))
}

#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    set_clipboard_text(&text)
}

fn set_clipboard_text(text: &str) -> Result<(), String> {
    unsafe {
        OpenClipboard(None).map_err(|error| format!("打开剪贴板失败: {error}"))?;
        let result = (|| {
            EmptyClipboard().map_err(|error| format!("清空剪贴板失败: {error}"))?;
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = utf16.len() * std::mem::size_of::<u16>();
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|error| format!("分配剪贴板内存失败: {error}"))?;
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                return Err("锁定剪贴板内存失败".into());
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, pointer as *mut u8, bytes);
            let _ = GlobalUnlock(memory);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0)))
                .map_err(|error| format!("写入剪贴板失败: {error}"))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
