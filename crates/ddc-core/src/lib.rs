use std::collections::HashMap;
use std::fmt;
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use windows::core::BOOL;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{GetLastError, HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

const VCP_BRIGHTNESS: u8 = 0x10;

const TRANSIENT_ERRORS: &[u32] = &[
    0xC0262582, // ERROR_GRAPHICS_I2C_ERROR_TRANSMITTING_DATA
    0xC0262583, // ERROR_GRAPHICS_I2C_ERROR_RECEIVING_DATA
    0xC0262586, // ERROR_GRAPHICS_DDCCI_MONITOR_RETURNED_INVALID_TIMING_STATUS_BYTE
    0xC0262589, // ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_COMMAND
    0xC026258A, // ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_LENGTH
    0xC026258B, // ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_CHECKSUM
];

const I2C_NOT_SUPPORTED: u32 = 0xC0262580;
const I2C_DEVICE_MISSING: u32 = 0xC0262581;
const VCP_NOT_SUPPORTED: u32 = 0xC0262584;
const MONITOR_GONE: u32 = 0xC026258D;

const RANGE_TTL: Duration = Duration::from_secs(30 * 60);

type RangeEntry = (u32, u32, Instant);

static RANGE_CACHE: OnceLock<Mutex<HashMap<String, RangeEntry>>> = OnceLock::new();

// Dxva2 talks to the GPU I2C bus; UI writes, hotkeys, restore and enumeration all
// run on different threads, and overlapping transactions can fail with I2C errors.
static DDC_LOCK: Mutex<()> = Mutex::new(());

fn ddc_guard() -> std::sync::MutexGuard<'static, ()> {
    DDC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn range_cache() -> &'static Mutex<HashMap<String, RangeEntry>> {
    RANGE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_range(id: &str) -> Option<(u32, u32)> {
    let cache = range_cache().lock().ok()?;
    let (min, max, stored_at) = cache.get(id)?;
    if stored_at.elapsed() > RANGE_TTL {
        return None;
    }
    Some((*min, *max))
}

fn store_range(id: &str, min: u32, max: u32) {
    if max <= min {
        return;
    }
    if let Ok(mut cache) = range_cache().lock() {
        cache.insert(id.to_string(), (min, max, Instant::now()));
    }
}

fn clear_range(id: &str) {
    if let Ok(mut cache) = range_cache().lock() {
        cache.remove(id);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DdcError {
    pub code: u32,
    pub context: String,
    pub transient: bool,
}

impl DdcError {
    pub fn new(code: u32, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
            transient: TRANSIENT_ERRORS.contains(&code),
        }
    }

    fn last_win32(context: impl Into<String>) -> Self {
        let code = unsafe { GetLastError().0 };
        Self::new(code, context)
    }

    pub fn name(&self) -> &'static str {
        error_name(self.code)
    }
}

impl fmt::Display for DdcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: 0x{:08X} {}", self.context, self.code, self.name())
    }
}

pub type DdcResult<T> = Result<T, DdcError>;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrightnessMethod {
    HighLevel,
    Vcp,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Brightness {
    pub raw: u32,
    pub min: u32,
    pub max: u32,
    pub percent: u32,
    pub method: BrightnessMethod,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOutcome {
    pub brightness: Option<Brightness>,
    pub error: Option<DdcError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub id: String,
    pub gdi_name: String,
    pub name: String,
    pub description: String,
    pub device_path: String,
    pub connection: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub brightness: Option<Brightness>,
    pub last_percent: Option<u32>,
    pub error: Option<DdcError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorList {
    pub monitors: Vec<MonitorInfo>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
struct PathInfo {
    gdi_name: String,
    friendly_name: String,
    device_path: String,
    connection: String,
}

struct PendingMonitor {
    id: String,
    gdi_name: String,
    name: String,
    description: String,
    device_path: String,
    connection: String,
    primary: bool,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    handle: isize,
}

pub fn enumerate() -> MonitorList {
    let _guard = ddc_guard();
    let mut notes = Vec::new();
    let paths = match query_paths() {
        Ok(paths) => paths,
        Err(error) => {
            notes.push(format!("QueryDisplayConfig: {error}"));
            Vec::new()
        }
    };

    let mut hmonitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_callback),
            LPARAM((&mut hmonitors as *mut Vec<HMONITOR>) as isize),
        );
    }

    let mut pending = Vec::new();
    let mut handles = Vec::new();
    for hmonitor in hmonitors {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _) }.as_bool() {
            notes.push(format!(
                "GetMonitorInfoW failed for HMONITOR {:?}",
                hmonitor.0
            ));
            continue;
        }
        let gdi_name = wide_to_string(&info.szDevice);
        let primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
        let rect = info.monitorInfo.rcMonitor;
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        let path = paths
            .iter()
            .find(|path| path.gdi_name.eq_ignore_ascii_case(&gdi_name))
            .cloned();

        let mut count = 0u32;
        if let Err(error) = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) }
        {
            notes.push(format!(
                "{gdi_name}: GetNumberOfPhysicalMonitorsFromHMONITOR failed: {error:?}"
            ));
            continue;
        }
        if count == 0 {
            notes.push(format!(
                "{gdi_name}: no physical monitors (virtual/remote/internal?), skipped"
            ));
            continue;
        }

        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if let Err(error) = unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut physical) } {
            notes.push(format!(
                "{gdi_name}: GetPhysicalMonitorsFromHMONITOR failed: {error:?}"
            ));
            continue;
        }

        for (index, handle) in physical.iter().enumerate() {
            let description_buffer = handle.szPhysicalMonitorDescription;
            let description = wide_to_string(&description_buffer);
            let device_path = path
                .as_ref()
                .map(|path| path.device_path.clone())
                .unwrap_or_default();
            let id_base = if device_path.is_empty() {
                gdi_name.clone()
            } else {
                device_path.clone()
            };
            handles.push(handle.hPhysicalMonitor.0 as isize);
            pending.push(PendingMonitor {
                id: format!("{id_base}#{index}"),
                gdi_name: gdi_name.clone(),
                name: path
                    .as_ref()
                    .map(|path| path.friendly_name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| description.clone()),
                description,
                device_path,
                connection: path
                    .as_ref()
                    .map(|path| path.connection.clone())
                    .unwrap_or_default(),
                primary,
                x: rect.left,
                y: rect.top,
                width,
                height,
                handle: handle.hPhysicalMonitor.0 as isize,
            });
        }
    }

    // 实测 Windows Dxva2 会把不同显示器的 DDC 访问全局串行化，
    // 并发读反而只增加线程开销，这里保持顺序读取。
    let reads: Vec<(Option<Brightness>, Option<DdcError>)> = pending
        .iter()
        .map(|monitor| read_brightness(handle_from(monitor.handle)))
        .collect();

    for raw in handles {
        unsafe {
            let _ = DestroyPhysicalMonitor(handle_from(raw));
        }
    }

    let monitors = pending
        .into_iter()
        .zip(reads)
        .map(|(monitor, (brightness, error))| {
            if let Some(brightness) = &brightness {
                if matches!(brightness.method, BrightnessMethod::HighLevel) {
                    store_range(&monitor.id, brightness.min, brightness.max);
                }
            }
            MonitorInfo {
                id: monitor.id,
                gdi_name: monitor.gdi_name,
                name: monitor.name,
                description: monitor.description,
                device_path: monitor.device_path,
                connection: monitor.connection,
                primary: monitor.primary,
                x: monitor.x,
                y: monitor.y,
                width: monitor.width,
                height: monitor.height,
                brightness,
                last_percent: None,
                error,
            }
        })
        .collect();

    MonitorList { monitors, notes }
}

fn handle_from(raw: isize) -> HANDLE {
    HANDLE(raw as *mut core::ffi::c_void)
}

pub fn set_brightness(id: &str, percent: u32) -> WriteOutcome {
    let _guard = ddc_guard();
    let percent = percent.min(100);
    let cached = cached_range(id);
    let result = with_monitor(id, |handle| {
        let (write, used_range) = write_brightness(handle, percent, cached);
        let brightness = if write.is_err() {
            thread::sleep(Duration::from_millis(80));
            read_brightness(handle).0
        } else {
            None
        };
        (write, brightness, used_range)
    });

    match result {
        Ok((Ok(()), brightness, used_range)) => {
            if let Some((min, max)) = used_range {
                store_range(id, min, max);
            }
            WriteOutcome {
                brightness,
                error: None,
            }
        }
        Ok((Err(error), brightness, _)) => {
            clear_range(id);
            WriteOutcome {
                brightness,
                error: Some(error),
            }
        }
        Err(error) => {
            clear_range(id);
            WriteOutcome {
                brightness: None,
                error: Some(error),
            }
        }
    }
}

pub fn get_brightness(id: &str) -> DdcResult<Brightness> {
    let _guard = ddc_guard();
    let (brightness, error) = with_monitor(id, read_brightness)?;
    match (brightness, error) {
        (Some(brightness), _) => Ok(brightness),
        (None, Some(error)) => Err(error),
        (None, None) => Err(DdcError::new(0, format!("monitor {id}: no brightness"))),
    }
}

fn with_monitor<T>(id: &str, mut operation: impl FnMut(HANDLE) -> T) -> DdcResult<T> {
    let paths = query_paths().unwrap_or_default();
    let mut hmonitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_callback),
            LPARAM((&mut hmonitors as *mut Vec<HMONITOR>) as isize),
        );
    }

    for hmonitor in hmonitors {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _) }.as_bool() {
            continue;
        }
        let gdi_name = wide_to_string(&info.szDevice);

        let mut count = 0u32;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) }.is_err()
            || count == 0
        {
            continue;
        }
        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut physical) }.is_err() {
            continue;
        }

        let path_device = paths
            .iter()
            .find(|path| path.gdi_name.eq_ignore_ascii_case(&gdi_name))
            .map(|path| path.device_path.clone())
            .unwrap_or_default();

        let mut matched = None;
        for (index, handle) in physical.iter().enumerate() {
            let id_base = if path_device.is_empty() {
                gdi_name.clone()
            } else {
                path_device.clone()
            };
            if format!("{id_base}#{index}") == id {
                matched = Some(operation(handle.hPhysicalMonitor));
                break;
            }
        }

        for handle in &physical {
            unsafe {
                let _ = DestroyPhysicalMonitor(handle.hPhysicalMonitor);
            }
        }
        if let Some(result) = matched {
            return Ok(result);
        }
    }

    Err(DdcError::new(0, format!("monitor {id} not found")))
}

fn read_brightness(handle: HANDLE) -> (Option<Brightness>, Option<DdcError>) {
    let high_level = retry(|| {
        let mut min = 0u32;
        let mut current = 0u32;
        let mut max = 0u32;
        let ok = unsafe { GetMonitorBrightness(handle, &mut min, &mut current, &mut max) };
        if ok == 0 {
            return Err(DdcError::last_win32("GetMonitorBrightness"));
        }
        let (min, max) = normalize_range(min, max);
        Ok(Brightness {
            raw: current,
            min,
            max,
            percent: to_percent(current, min, max),
            method: BrightnessMethod::HighLevel,
        })
    });

    match high_level {
        Ok(brightness) => (Some(brightness), None),
        Err(high_level_error) => {
            let vcp = retry(|| {
                let mut current = 0u32;
                let mut max = 0u32;
                let ok = unsafe {
                    GetVCPFeatureAndVCPFeatureReply(
                        handle,
                        VCP_BRIGHTNESS,
                        None,
                        &mut current,
                        Some(&mut max),
                    )
                };
                if ok == 0 {
                    return Err(DdcError::last_win32("GetVCPFeature(0x10)"));
                }
                let (min, max) = normalize_range(0, max);
                Ok(Brightness {
                    raw: current,
                    min,
                    max,
                    percent: to_percent(current, min, max),
                    method: BrightnessMethod::Vcp,
                })
            });
            match vcp {
                Ok(brightness) => (Some(brightness), None),
                Err(_) => (None, Some(high_level_error)),
            }
        }
    }
}

fn write_brightness(
    handle: HANDLE,
    percent: u32,
    cached: Option<(u32, u32)>,
) -> (DdcResult<()>, Option<(u32, u32)>) {
    // 每次写入前读 min/max 要多花一次 DDC 往返，缓存后同一显示器可直接换算。
    let range = cached.or_else(|| {
        read_range(handle)
            .ok()
            .map(|(min, max)| normalize_range(min, max))
    });
    let (min, max) = range.unwrap_or((0, 100));
    let raw = to_raw(percent, min, max);

    let high_level = retry(|| {
        let ok = unsafe { SetMonitorBrightness(handle, raw) };
        if ok == 0 {
            return Err(DdcError::last_win32("SetMonitorBrightness"));
        }
        Ok(())
    });

    match high_level {
        Ok(()) => (Ok(()), range),
        Err(high_level_error) => {
            let vcp_max = read_vcp_range(handle).map(|(_, max)| max).unwrap_or(100);
            let vcp_raw = to_raw(percent, 0, normalize_range(0, vcp_max).1);
            match retry(|| {
                let ok = unsafe { SetVCPFeature(handle, VCP_BRIGHTNESS, vcp_raw) };
                if ok == 0 {
                    return Err(DdcError::last_win32("SetVCPFeature(0x10)"));
                }
                Ok(())
            }) {
                Ok(()) => (Ok(()), None),
                Err(_) => (Err(high_level_error), None),
            }
        }
    }
}

fn read_range(handle: HANDLE) -> DdcResult<(u32, u32)> {
    retry(|| {
        let mut min = 0u32;
        let mut current = 0u32;
        let mut max = 0u32;
        let ok = unsafe { GetMonitorBrightness(handle, &mut min, &mut current, &mut max) };
        if ok == 0 {
            return Err(DdcError::last_win32("GetMonitorBrightness"));
        }
        Ok((min, max))
    })
}

fn read_vcp_range(handle: HANDLE) -> DdcResult<(u32, u32)> {
    retry(|| {
        let mut current = 0u32;
        let mut max = 0u32;
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                handle,
                VCP_BRIGHTNESS,
                None,
                &mut current,
                Some(&mut max),
            )
        };
        if ok == 0 {
            return Err(DdcError::last_win32("GetVCPFeature(0x10)"));
        }
        Ok((0, max))
    })
}

fn retry<T, F>(mut operation: F) -> DdcResult<T>
where
    F: FnMut() -> DdcResult<T>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !error.transient || attempts >= 3 {
                    return Err(error);
                }
                thread::sleep(Duration::from_millis(50 * attempts as u64));
            }
        }
    }
}

fn query_paths() -> DdcResult<Vec<PathInfo>> {
    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let status =
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count);
        if status.0 != 0 {
            return Err(DdcError::new(status.0, "GetDisplayConfigBufferSizes"));
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        );
        if status.0 != 0 {
            return Err(DdcError::new(status.0, "QueryDisplayConfig"));
        }
        paths.truncate(path_count as usize);

        let mut result = Vec::with_capacity(paths.len());
        for path in paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            let source_status = DisplayConfigGetDeviceInfo(&mut source.header);

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            let target_status = DisplayConfigGetDeviceInfo(&mut target.header);

            result.push(PathInfo {
                gdi_name: if source_status == 0 {
                    wide_to_string(&source.viewGdiDeviceName)
                } else {
                    String::new()
                },
                friendly_name: if target_status == 0 {
                    wide_to_string(&target.monitorFriendlyDeviceName)
                } else {
                    String::new()
                },
                device_path: if target_status == 0 {
                    wide_to_string(&target.monitorDevicePath)
                } else {
                    String::new()
                },
                connection: connection_name(path.targetInfo.outputTechnology),
            });
        }
        Ok(result)
    }
}

unsafe extern "system" fn enum_monitor_callback(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<HMONITOR>);
    monitors.push(hmonitor);
    BOOL(1)
}

fn connection_name(technology: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> String {
    if technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI {
        "HDMI".into()
    } else if technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL
        || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
    {
        "DisplayPort".into()
    } else if technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL {
        "USB-C".into()
    } else if technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI {
        "DVI".into()
    } else if technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 {
        "VGA".into()
    } else {
        String::new()
    }
}

fn normalize_range(min: u32, max: u32) -> (u32, u32) {
    if max <= min {
        (0, 100)
    } else {
        (min, max)
    }
}

fn to_percent(raw: u32, min: u32, max: u32) -> u32 {
    if max <= min {
        return raw.min(100);
    }
    let raw = raw.clamp(min, max);
    (((raw - min) as u64 * 100 + (max - min) as u64 / 2) / (max - min) as u64) as u32
}

fn to_raw(percent: u32, min: u32, max: u32) -> u32 {
    if max <= min {
        return percent.min(100);
    }
    let percent = percent.min(100);
    min + ((max - min) as u64 * percent as u64 / 100) as u32
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

pub fn error_name(code: u32) -> &'static str {
    match code {
        0 => "NO_ERROR",
        I2C_NOT_SUPPORTED => "ERROR_GRAPHICS_I2C_NOT_SUPPORTED",
        I2C_DEVICE_MISSING => "ERROR_GRAPHICS_I2C_DEVICE_DOES_NOT_EXIST",
        0xC0262582 => "ERROR_GRAPHICS_I2C_ERROR_TRANSMITTING_DATA",
        0xC0262583 => "ERROR_GRAPHICS_I2C_ERROR_RECEIVING_DATA",
        VCP_NOT_SUPPORTED => "ERROR_GRAPHICS_DDCCI_VCP_NOT_SUPPORTED",
        0xC0262585 => "ERROR_GRAPHICS_DDCCI_INVALID_DATA",
        0xC0262586 => "ERROR_GRAPHICS_DDCCI_MONITOR_RETURNED_INVALID_TIMING_STATUS_BYTE",
        0xC0262589 => "ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_COMMAND",
        0xC026258A => "ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_LENGTH",
        0xC026258B => "ERROR_GRAPHICS_DDCCI_INVALID_MESSAGE_CHECKSUM",
        MONITOR_GONE => "ERROR_GRAPHICS_MONITOR_NO_LONGER_EXISTS",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_maps_into_range() {
        assert_eq!(to_percent(100, 0, 100), 100);
        assert_eq!(to_percent(50, 0, 100), 50);
        assert_eq!(to_percent(0, 0, 100), 0);
        assert_eq!(to_percent(100, 0, 200), 50);
        assert_eq!(to_percent(150, 0, 200), 75);
        assert_eq!(to_raw(75, 0, 200), 150);
        assert_eq!(to_raw(50, 20, 120), 70);
        assert_eq!(to_raw(100, 0, 200), 200);
    }

    #[test]
    fn percent_clamps_bad_raw() {
        assert_eq!(to_percent(300, 0, 100), 100);
        assert_eq!(to_percent(50, 100, 100), 50);
    }

    #[test]
    fn transient_errors_are_classified() {
        assert!(DdcError::new(0xC0262582, "x").transient);
        assert!(DdcError::new(0xC0262583, "x").transient);
        assert!(!DdcError::new(VCP_NOT_SUPPORTED, "x").transient);
        assert!(!DdcError::new(MONITOR_GONE, "x").transient);
        assert!(!DdcError::new(0, "x").transient);
    }

    #[test]
    fn error_names_are_stable() {
        assert_eq!(
            error_name(0xC0262582),
            "ERROR_GRAPHICS_I2C_ERROR_TRANSMITTING_DATA"
        );
        assert_eq!(
            error_name(VCP_NOT_SUPPORTED),
            "ERROR_GRAPHICS_DDCCI_VCP_NOT_SUPPORTED"
        );
        assert_eq!(error_name(1), "UNKNOWN");
    }

    #[test]
    fn range_normalization_falls_back() {
        assert_eq!(normalize_range(0, 100), (0, 100));
        assert_eq!(normalize_range(0, 0), (0, 100));
        assert_eq!(normalize_range(10, 10), (0, 100));
    }
}
