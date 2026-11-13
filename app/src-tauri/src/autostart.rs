use std::slice;

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const VALUE_NAME: PCWSTR = w!("LumenTray");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APPROVED_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");

fn open_key(path: PCWSTR, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path, Some(0), access, &mut key).is_err() {
            return None;
        }
        Some(key)
    }
}

fn read_raw() -> Option<String> {
    unsafe {
        let key = open_key(RUN_KEY, KEY_READ)?;
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = 0u32;
        let status = RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        );
        if status.is_err() || size == 0 {
            let _ = RegCloseKey(key);
            return None;
        }
        let mut buffer = vec![0u16; size as usize / 2 + 1];
        let mut read = size;
        let status = RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            Some(&mut kind),
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut read),
        );
        let _ = RegCloseKey(key);
        if status.is_err() {
            return None;
        }
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }
}

fn delete_value(path: PCWSTR) {
    unsafe {
        let Some(key) = open_key(path, KEY_SET_VALUE) else {
            return;
        };
        let status = RegDeleteValueW(key, VALUE_NAME);
        if status.is_err() && status.0 != ERROR_FILE_NOT_FOUND.0 {
            crate::trace(&format!(
                "[autostart] delete {} failed: {}",
                path.to_string().unwrap_or_default(),
                status.0
            ));
        }
        let _ = RegCloseKey(key);
    }
}

/// True only when the Run entry exists and points at this executable.
pub fn is_enabled() -> bool {
    let Some(stored) = read_raw() else {
        return false;
    };
    let stored = stored.trim().trim_matches('"');
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    stored.eq_ignore_ascii_case(&exe.to_string_lossy())
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if !enabled {
        delete_value(RUN_KEY);
        // Remove the Task Manager "disabled" marker so enabling later is not silently ignored.
        delete_value(APPROVED_KEY);
        return Ok(());
    }

    let exe = std::env::current_exe().map_err(|error| format!("取不到程序路径: {error}"))?;
    let Some(key) = open_key(RUN_KEY, KEY_SET_VALUE) else {
        return Err("无法打开开机启动注册表项".into());
    };
    let command = format!("\"{}\"", exe.display());
    let utf16: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe {
        slice::from_raw_parts(
            utf16.as_ptr() as *const u8,
            utf16.len() * std::mem::size_of::<u16>(),
        )
    };
    let status = unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)) };
    let _ = unsafe { RegCloseKey(key) };
    status
        .ok()
        .map_err(|error| format!("写入开机启动失败: {error}"))?;
    delete_value(APPROVED_KEY);
    Ok(())
}
