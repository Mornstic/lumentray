mod autostart;
mod cache;
mod diagnostics;
mod hotkeys;
mod logging;
mod osd;
mod settings;

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use settings::{SettingsPatch, SettingsSnapshot, SettingsStore, ThemeChoice};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Theme, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DispatchMessageW, GetMessageW,
    MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_POWERSETTINGCHANGE, RegisterClassW,
    SC_MONITORPOWER, TranslateMessage, WM_DEVICECHANGE, WM_DISPLAYCHANGE, WM_POWERBROADCAST,
    WM_SETTINGCHANGE, WM_SYSCOMMAND, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::w;

const FLYOUT_LABEL: &str = "flyout";
const SETTINGS_LABEL: &str = "settings";
/// 透明窗内留出的阴影画布；卡片本身仍保持 380 宽。
const WINDOW_GUTTER: f64 = 20.0;
const FLYOUT_WIDTH: f64 = 380.0;
const FLYOUT_HEIGHT: f64 = 548.0;
const FLYOUT_WINDOW_WIDTH: f64 = FLYOUT_WIDTH + WINDOW_GUTTER * 2.0;
const FLYOUT_WINDOW_HEIGHT: f64 = FLYOUT_HEIGHT + WINDOW_GUTTER * 2.0;
const SETTINGS_WIDTH: f64 = 720.0;
const SETTINGS_HEIGHT: f64 = 560.0;

static LAST_CLOSE: Mutex<Option<Instant>> = Mutex::new(None);
static QUITTING: AtomicBool = AtomicBool::new(false);
static EVENT_APP: OnceLock<AppHandle> = OnceLock::new();
static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);
static DISPLAY_ON_SEQ: AtomicU64 = AtomicU64::new(0);
static USER_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
static RESTORE_RUNNING: AtomicBool = AtomicBool::new(false);
/// 当前托盘图标对应的任务栏配色（true = 浅色任务栏，使用深色图标）。
static TRAY_LIGHT_TASKBAR: AtomicBool = AtomicBool::new(false);

pub(crate) fn trace(message: &str) {
    logging::write(message);
}

pub(crate) fn note_user_write() {
    USER_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "system" fn event_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(app) = EVENT_APP.get()
        && hotkeys::handle_message(app, msg, wparam, lparam)
    {
        return LRESULT(0);
    }
    // 系统主题切换会广播 WM_SETTINGCHANGE（ImmersiveColorSet）。托盘图标是静态位图，
    // 不会像 WebView 的 prefers-color-scheme 那样自动重绘，必须按新的任务栏配色换图。
    if msg == WM_SETTINGCHANGE
        && let Some(app) = EVENT_APP.get().cloned()
    {
        let _ = std::thread::Builder::new()
            .name("lumentray-tray-theme".into())
            .spawn(move || refresh_tray_icon(&app));
    }
    let (delay, restore) = match msg {
        WM_DISPLAYCHANGE | WM_DEVICECHANGE => (Some(600), false),
        WM_POWERBROADCAST => match wparam.0 as u32 {
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => (Some(1200), true),
            // 显示器单独待机/上电不会产生系统 resume；部分显示器在 DPMS 待机时
            // 会丢掉 DDC 设定的亮度，因此屏幕重新点亮后也要走一次恢复。
            PBT_POWERSETTINGCHANGE => {
                let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting != GUID_CONSOLE_DISPLAY_STATE || setting.DataLength < 4 {
                    (None, false)
                } else {
                    match setting.Data[0] {
                        0 => {
                            trace("[events] display off");
                            (None, false)
                        }
                        1 => {
                            trace("[events] display on -> restore");
                            schedule_display_restore_followup();
                            (Some(6000), true)
                        }
                        _ => (None, false),
                    }
                }
            }
            _ => (None, false),
        },
        WM_SYSCOMMAND => {
            let command = (wparam.0 as u32) & 0xFFF0;
            if command == SC_MONITORPOWER && lparam.0 as i32 == -1 {
                trace("[events] monitor power on -> restore");
                schedule_display_restore_followup();
                (Some(6000), true)
            } else {
                (None, false)
            }
        }
        _ => (None, false),
    };
    if let Some(delay) = delay {
        schedule_monitor_refresh(delay, restore);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn notify_monitor_windows(app: &AppHandle) {
    if app.get_webview_window(FLYOUT_LABEL).is_some()
        || app.get_webview_window(SETTINGS_LABEL).is_some()
    {
        let _ = app.emit("monitors-changed", ());
    }
}

fn schedule_display_restore_followup() {
    let Some(app) = EVENT_APP.get().cloned() else {
        return;
    };
    let seq = DISPLAY_ON_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let user_seq = USER_WRITE_SEQ.load(Ordering::Relaxed);
    let _ = std::thread::Builder::new()
        .name("lumentray-restore-followup".into())
        .spawn(move || {
            // 部分显示器点亮后先报旧值，几秒后才回落；6 秒的首次恢复可能已经
            // 错过这个窗口，12 秒再核对一次，用户中途手动调节则不再覆盖。
            std::thread::sleep(Duration::from_millis(12000));
            if DISPLAY_ON_SEQ.load(Ordering::Relaxed) != seq {
                return;
            }
            if USER_WRITE_SEQ.load(Ordering::Relaxed) != user_seq {
                trace("[restore] followup skipped: user wrote meanwhile");
                return;
            }
            restore_saved_brightness(&app, false);
        });
}

fn schedule_monitor_refresh(delay_ms: u64, restore: bool) {
    let Some(app) = EVENT_APP.get().cloned() else {
        return;
    };
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = std::thread::Builder::new()
        .name("lumentray-event-debounce".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            if EVENT_SEQ.load(Ordering::Relaxed) != seq {
                return;
            }
            if restore {
                trace("[events] resume -> restore saved brightness");
                restore_saved_brightness(&app, false);
            }
            notify_monitor_windows(&app);
        });
}

fn spawn_event_window(app: AppHandle) {
    let worker_app = app.clone();
    let _ = EVENT_APP.set(app);
    let _ = std::thread::Builder::new()
        .name("lumentray-events".into())
        .spawn(move || unsafe {
            let instance = match GetModuleHandleW(None) {
                Ok(module) => HINSTANCE::from(module),
                Err(error) => {
                    trace(&format!("[events] GetModuleHandleW failed: {error}"));
                    return;
                }
            };
            let class_name = w!("LumenTrayEvents");
            let window_class = WNDCLASSW {
                lpfnWndProc: Some(event_wnd_proc),
                hInstance: instance,
                lpszClassName: class_name,
                ..Default::default()
            };
            if RegisterClassW(&window_class) == 0 {
                trace("[events] RegisterClassW failed");
            }
            match CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class_name,
                w!("LumenTray Events"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            ) {
                Ok(hwnd) => {
                    trace(&format!("[events] hidden window {:?}", hwnd.0));
                    if let Err(error) = RegisterPowerSettingNotification(
                        HANDLE(hwnd.0),
                        &GUID_CONSOLE_DISPLAY_STATE,
                        DEVICE_NOTIFY_WINDOW_HANDLE,
                    ) {
                        trace(&format!("[events] register display power failed: {error}"));
                    }
                    hotkeys::set_window(hwnd);
                    hotkeys::init(&worker_app);
                }
                Err(error) => {
                    trace(&format!("[events] CreateWindowExW failed: {error}"));
                    return;
                }
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
}

#[tauri::command]
async fn list_monitors(app: AppHandle) -> ddc_core::MonitorList {
    let started = Instant::now();
    let mut list = tauri::async_runtime::spawn_blocking(ddc_core::enumerate)
        .await
        .unwrap_or_else(|error| ddc_core::MonitorList {
            monitors: Vec::new(),
            notes: vec![format!("monitor enumeration task failed: {error}")],
        });
    trace(&format!(
        "[perf] enumerated {} monitors in {}ms",
        list.monitors.len(),
        started.elapsed().as_millis()
    ));
    let store = app.state::<SettingsStore>();
    #[cfg(debug_assertions)]
    if let Ok(mode) = std::env::var("LUMENTRAY_SIMULATE_ERROR") {
        simulate_errors(&mut list, &mode);
    }
    for monitor in &mut list.monitors {
        if monitor.brightness.is_none() {
            monitor.last_percent = store.last_brightness(&monitor.id);
        }
    }
    cache::remember(&list);
    list
}

#[tauri::command]
fn get_monitor_snapshot(app: AppHandle) -> Option<ddc_core::MonitorList> {
    let mut monitors = cache::cached_monitors()?;
    let store = app.state::<SettingsStore>();
    for monitor in &mut monitors {
        if monitor.brightness.is_none() {
            monitor.last_percent = store.last_brightness(&monitor.id);
        }
    }
    Some(ddc_core::MonitorList {
        monitors,
        notes: Vec::new(),
    })
}

#[cfg(debug_assertions)]
fn simulate_errors(list: &mut ddc_core::MonitorList, mode: &str) {
    if mode == "empty" {
        list.monitors.clear();
        return;
    }
    let last = list.monitors.len().saturating_sub(1);
    for (index, monitor) in list.monitors.iter_mut().enumerate() {
        if mode == "all" || (mode == "one" && index == last) {
            monitor.brightness = None;
            monitor.error = Some(ddc_core::DdcError::new(
                0xC0262582,
                "simulated DDC/CI timeout",
            ));
        }
    }
}

#[tauri::command]
async fn set_brightness(app: AppHandle, id: String, percent: u32) -> ddc_core::WriteOutcome {
    note_user_write();

    #[cfg(debug_assertions)]
    if std::env::var_os("LUMENTRAY_SIMULATE_WRITE_ERROR").is_some() {
        let read_id = id.clone();
        let actual =
            tauri::async_runtime::spawn_blocking(move || ddc_core::get_brightness(&read_id))
                .await
                .ok()
                .and_then(Result::ok);
        if let Some(brightness) = actual {
            app.state::<SettingsStore>()
                .record_last_brightness(&id, brightness.percent);
            hotkeys::note_brightness(&id, brightness.percent);
        }
        return ddc_core::WriteOutcome {
            brightness: actual,
            error: Some(ddc_core::DdcError::new(
                0xC0262582,
                "simulated write failure",
            )),
        };
    }

    let write_id = id.clone();
    let outcome =
        tauri::async_runtime::spawn_blocking(move || ddc_core::set_brightness(&write_id, percent))
            .await
            .unwrap_or_else(|error| ddc_core::WriteOutcome {
                brightness: None,
                error: Some(ddc_core::DdcError::new(
                    0,
                    format!("brightness task failed: {error}"),
                )),
            });
    let store = app.state::<SettingsStore>();
    if let Some(brightness) = outcome.brightness {
        store.record_last_brightness(&id, brightness.percent);
        hotkeys::note_brightness(&id, brightness.percent);
    } else if outcome.error.is_none() {
        let percent = percent.min(100);
        store.record_last_brightness(&id, percent);
        hotkeys::note_brightness(&id, percent);
    }
    outcome
}

struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        RESTORE_RUNNING.store(false, Ordering::Release);
    }
}

fn restore_saved_brightness(app: &AppHandle, force: bool) {
    restore_saved_brightness_with_retry(app, force, true);
}

fn restore_saved_brightness_with_retry(app: &AppHandle, force: bool, allow_retry: bool) {
    if RESTORE_RUNNING.swap(true, Ordering::AcqRel) {
        trace("[restore] already running, skipped");
        return;
    }
    let _guard = RestoreGuard;

    let enabled = force || { app.state::<SettingsStore>().snapshot().restore_on_wake };
    if !enabled {
        return;
    }

    let start_seq = USER_WRITE_SEQ.load(Ordering::Relaxed);
    let started = Instant::now();
    let list = ddc_core::enumerate();
    trace(&format!(
        "[restore] enumerate {} monitors in {}ms",
        list.monitors.len(),
        started.elapsed().as_millis()
    ));
    cache::remember(&list);
    let store = app.state::<SettingsStore>();
    let (mut restored, mut deferred, mut failed) = (0usize, 0usize, 0usize);

    for monitor in list.monitors {
        if monitor.brightness.is_none() {
            if store.last_brightness(&monitor.id).is_some() {
                deferred += 1;
                trace(&format!("[restore] {} unreadable, deferred", monitor.name));
            }
            continue;
        }
        let Some(saved) = store.last_brightness(&monitor.id) else {
            continue;
        };
        if monitor.brightness.map(|brightness| brightness.percent) == Some(saved) {
            continue;
        }
        if USER_WRITE_SEQ.load(Ordering::Relaxed) != start_seq {
            trace("[restore] aborted: user changed brightness during restore");
            break;
        }
        let outcome = ddc_core::set_brightness(&monitor.id, saved);
        match (&outcome.error, outcome.brightness) {
            (None, _) => {
                restored += 1;
                trace(&format!("[restore] {} -> {saved}%", monitor.name));
                hotkeys::note_brightness(&monitor.id, saved);
            }
            (Some(error), _) => {
                failed += 1;
                trace(&format!(
                    "[restore] {} -> {saved}% failed: 0x{:08X} {}",
                    monitor.name, error.code, error.context
                ));
            }
        }
    }
    if restored > 0 || failed > 0 {
        trace(&format!(
            "[restore] done: {restored} restored, {failed} failed"
        ));
    }
    if restored > 0 {
        // 打开中的飞窗可能早于恢复完成就已读取过旧值
        notify_monitor_windows(app);
    }
    if (deferred > 0 || failed > 0) && allow_retry {
        trace(&format!(
            "[restore] {deferred} deferred, {failed} failed; retrying in 2500ms"
        ));
        let retry_app = app.clone();
        let _ = std::thread::Builder::new()
            .name("lumentray-restore-retry".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(2500));
                restore_saved_brightness_with_retry(&retry_app, force, false);
            });
    }
}

fn theme_for(choice: ThemeChoice) -> Option<Theme> {
    match choice {
        ThemeChoice::System => None,
        ThemeChoice::Light => Some(Theme::Light),
        ThemeChoice::Dark => Some(Theme::Dark),
    }
}

fn apply_current_theme(app: &AppHandle, window: &WebviewWindow) {
    let choice = app.state::<SettingsStore>().snapshot().theme;
    if let Err(error) = window.set_theme(theme_for(choice)) {
        trace(&format!(
            "[theme] set on {} failed: {error}",
            window.label()
        ));
    }
}

fn apply_theme_to_windows(app: &AppHandle) {
    let choice = app.state::<SettingsStore>().snapshot().theme;
    for label in [FLYOUT_LABEL, SETTINGS_LABEL] {
        if let Some(window) = app.get_webview_window(label)
            && let Err(error) = window.set_theme(theme_for(choice))
        {
            trace(&format!("[theme] set on {label} failed: {error}"));
        }
    }
}

#[tauri::command]
fn close_flyout(window: WebviewWindow) -> tauri::Result<()> {
    window.close()
}

#[tauri::command]
fn resize_flyout(window: WebviewWindow, height: f64) -> tauri::Result<()> {
    let scale = window.scale_factor()?;
    let old_size = window.outer_size()?;
    let monitor = window.current_monitor()?.or(window.primary_monitor()?);

    let min_height = (160.0 + WINDOW_GUTTER * 2.0) * scale;
    let max_height = monitor
        .as_ref()
        .map(|monitor| monitor.work_area().size.height.max(1) as f64)
        .unwrap_or(640.0 * scale)
        .max(min_height);
    let height = (height.clamp(160.0 + WINDOW_GUTTER * 2.0, 640.0 + WINDOW_GUTTER * 2.0) * scale)
        .clamp(min_height, max_height)
        .round() as u32;
    let width = (FLYOUT_WINDOW_WIDTH * scale).round() as u32;
    window.set_size(PhysicalSize::new(width, height))?;

    if let (Some(monitor), Ok(position)) = (monitor, window.outer_position()) {
        let area = monitor.work_area();
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        let area_left = area.position.x;
        let area_top = area.position.y;
        let area_right = area.position.x + area.size.width as i32;
        let area_bottom = area.position.y + area.size.height as i32;
        let taskbar_top = area_top > monitor_position.y;
        let taskbar_left = area_left > monitor_position.x;
        let taskbar_right = area_right < monitor_position.x + monitor_size.width as i32;

        let height = height as i32;
        let old_height = old_size.height as i32;
        let mut y = if taskbar_top {
            area_top
        } else if taskbar_left || taskbar_right {
            position.y + (old_height - height) / 2
        } else {
            area_bottom - height
        };
        let min_y = area_top;
        let max_y = (area_bottom - height).max(min_y);
        y = y.clamp(min_y, max_y);
        let min_x = area_left;
        let max_x = (area_right - width as i32).max(min_x);
        let x = position.x.clamp(min_x, max_x);
        window.set_position(PhysicalPosition::new(x, y))?;
    }
    Ok(())
}

fn create_settings_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let started = Instant::now();
    let window = WebviewWindowBuilder::new(
        app,
        SETTINGS_LABEL,
        WebviewUrl::App("index.html?view=settings".into()),
    )
    .title("LumenTray 设置")
    .inner_size(
        SETTINGS_WIDTH + WINDOW_GUTTER * 2.0,
        SETTINGS_HEIGHT + WINDOW_GUTTER * 2.0,
    )
    .resizable(false)
    .maximizable(false)
    .minimizable(true)
    .decorations(false)
    .transparent(true)
    // 原生阴影会对整个窗口矩形再画一圈边框；阴影改由 CSS 在透明 gutter 里画。
    .shadow(false)
    .always_on_top(false)
    .skip_taskbar(false)
    .visible(false)
    .focused(true)
    .build()?;

    apply_current_theme(app, &window);
    window.center()?;

    // 透明窗必须在 WebView 首帧之后才 show，否则合成层不会挂载，整窗一直是空的。
    let fallback = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        if !fallback.is_visible().unwrap_or(false) {
            trace("[settings] reveal fallback");
            let _ = fallback.show();
            let _ = fallback.set_focus();
        }
    });

    trace(&format!(
        "[perf] settings window built {}ms",
        started.elapsed().as_millis()
    ));
    Ok(window)
}

fn open_settings_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        window.set_focus()?;
        return Ok(());
    }
    if let Some(flyout) = app.get_webview_window(FLYOUT_LABEL) {
        // 先建设置窗：焦点转移后飞窗自身也会走失焦关闭，避免中间一段空白
        create_settings_window(app)?;
        *LAST_CLOSE.lock().unwrap() = Some(Instant::now());
        let _ = flyout.close();
        return Ok(());
    }
    create_settings_window(app)?;
    Ok(())
}

#[tauri::command]
async fn open_settings(app: AppHandle) -> Result<(), String> {
    // 建窗会等待主线程事件循环，同步命令在主线程执行会自锁
    open_settings_window(&app).map_err(|error| error.to_string())
}

#[tauri::command]
fn close_settings(window: WebviewWindow) -> tauri::Result<()> {
    window.close()
}

#[tauri::command]
fn get_settings(app: AppHandle) -> SettingsSnapshot {
    app.state::<SettingsStore>().view(&app)
}

#[tauri::command]
fn save_settings(app: AppHandle, patch: SettingsPatch) -> Result<SettingsSnapshot, String> {
    let store = app.state::<SettingsStore>();
    store.apply_patch(&patch)?;
    apply_theme_to_windows(&app);
    // 在事件线程上重注册，随后把注册结果（含冲突）带回设置页。
    hotkeys::request_reload(Duration::from_millis(800));
    let _ = app.emit("settings-changed", ());
    Ok(store.view(&app))
}

fn place_flyout(window: &WebviewWindow, anchor: PhysicalPosition<f64>) -> tauri::Result<()> {
    let app = window.app_handle();
    let monitor = app
        .monitor_from_point(anchor.x, anchor.y)?
        .or(app.primary_monitor()?);
    let Some(monitor) = monitor else {
        return Ok(());
    };

    let scale = monitor.scale_factor();
    let width = (FLYOUT_WINDOW_WIDTH * scale).round() as i32;
    let height = (FLYOUT_WINDOW_HEIGHT * scale).round() as i32;
    let area = monitor.work_area();
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();

    let area_left = area.position.x;
    let area_top = area.position.y;
    let area_right = area.position.x + area.size.width as i32;
    let area_bottom = area.position.y + area.size.height as i32;

    let taskbar_top = area_top > monitor_position.y;
    let taskbar_left = area_left > monitor_position.x;
    let taskbar_right = area_right < monitor_position.x + monitor_size.width as i32;

    let mut x = (anchor.x - width as f64 / 2.0).round() as i32;
    let mut y = area_bottom - height;

    if taskbar_top {
        y = area_top;
    } else if taskbar_left {
        x = area_left;
        y = (anchor.y - height as f64 / 2.0).round() as i32;
    } else if taskbar_right {
        x = area_right - width;
        y = (anchor.y - height as f64 / 2.0).round() as i32;
    }

    let min_x = area_left;
    let max_x = (area_right - width).max(min_x);
    let min_y = area_top;
    let max_y = (area_bottom - height).max(min_y);

    x = x.clamp(min_x, max_x);
    y = y.clamp(min_y, max_y);

    window.set_size(PhysicalSize::new(width as u32, height as u32))?;
    window.set_position(PhysicalPosition::new(x, y))?;
    Ok(())
}

fn create_flyout(app: &AppHandle, anchor: PhysicalPosition<f64>) -> tauri::Result<()> {
    let started = Instant::now();
    let window = WebviewWindowBuilder::new(app, FLYOUT_LABEL, WebviewUrl::App("index.html".into()))
        .title("LumenTray")
        .inner_size(FLYOUT_WINDOW_WIDTH, FLYOUT_WINDOW_HEIGHT)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .transparent(true)
        // 原生阴影会对整个窗口矩形再画一圈边框；阴影改由 CSS 在透明 gutter 里画。
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .focused(true)
        .build()?;
    trace(&format!("[perf] build {}ms", started.elapsed().as_millis()));

    apply_current_theme(app, &window);
    place_flyout(&window, anchor)?;

    // 前端首屏排版完成后调用 reveal_flyout；超时兜底避免点了托盘没反应。
    let fallback = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        if !fallback.is_visible().unwrap_or(false) {
            trace("[flyout] reveal fallback");
            let _ = fallback.show();
            let _ = fallback.set_focus();
        }
    });

    #[cfg(debug_assertions)]
    let auto_close = std::env::var_os("LUMENTRAY_NO_AUTOCLOSE").is_none();
    #[cfg(not(debug_assertions))]
    let auto_close = true;
    let on_focus_lost = window.clone();
    let opened_at = Instant::now();
    let focused_once = std::sync::atomic::AtomicBool::new(false);
    if auto_close {
        window.on_window_event(move |event| match event {
            WindowEvent::Focused(true) => {
                focused_once.store(true, std::sync::atomic::Ordering::Relaxed)
            }
            WindowEvent::Focused(false)
                if focused_once.load(std::sync::atomic::Ordering::Relaxed)
                    && opened_at.elapsed() >= Duration::from_millis(250) =>
            {
                trace("[flyout] focus lost -> close");
                *LAST_CLOSE.lock().unwrap() = Some(Instant::now());
                let _ = on_focus_lost.close();
            }
            _ => {}
        });
    }

    Ok(())
}

#[tauri::command]
fn reveal_flyout(window: WebviewWindow) -> tauri::Result<()> {
    if window.is_visible().unwrap_or(false) {
        return Ok(());
    }
    window.show()?;
    window.set_focus()?;
    trace("[flyout] revealed");
    Ok(())
}

#[tauri::command]
fn reveal_settings(window: WebviewWindow) -> tauri::Result<()> {
    if window.is_visible().unwrap_or(false) {
        return Ok(());
    }
    window.show()?;
    window.set_focus()?;
    trace("[settings] revealed");
    Ok(())
}

fn toggle_flyout(app: &AppHandle, anchor: PhysicalPosition<f64>) {
    trace(&format!("[tray] toggle at {:.0},{:.0}", anchor.x, anchor.y));
    if let Some(window) = app.get_webview_window(FLYOUT_LABEL) {
        *LAST_CLOSE.lock().unwrap() = Some(Instant::now());
        let _ = window.close();
        return;
    }

    let recently_closed = LAST_CLOSE
        .lock()
        .unwrap()
        .map(|closed_at| closed_at.elapsed() < Duration::from_millis(300))
        .unwrap_or(false);
    if recently_closed {
        trace("[tray] ignored: recently closed");
        return;
    }

    match create_flyout(app, anchor) {
        Ok(()) => trace("[tray] flyout created"),
        Err(error) => trace(&format!("[tray] failed to create flyout: {error}")),
    }
}

fn show_flyout(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(FLYOUT_LABEL) {
        if window.is_visible().unwrap_or(false) {
            let _ = window.set_focus();
        }
        return;
    }
    let anchor = app
        .cursor_position()
        .unwrap_or(PhysicalPosition::new(0.0, 0.0));
    if let Err(error) = create_flyout(app, anchor) {
        trace(&format!("[tray] failed to show flyout: {error}"));
    }
}

fn system_uses_light_taskbar() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    status.0 == 0 && value == 1
}

fn tray_icon_bytes(light_taskbar: bool) -> &'static [u8] {
    if light_taskbar {
        include_bytes!("../icons/tray/tray-dark-32.png")
    } else {
        include_bytes!("../icons/tray/tray-light-32.png")
    }
}

/// 按当前任务栏配色重设托盘图标。只在读数变化时调用 set_icon，避免
/// WM_SETTINGCHANGE 连续广播时反复向主线程派发；失败则回滚状态以便下次重试。
fn refresh_tray_icon(app: &AppHandle) {
    let light = system_uses_light_taskbar();
    let previous = TRAY_LIGHT_TASKBAR.load(Ordering::Relaxed);
    if previous == light {
        return;
    }
    if TRAY_LIGHT_TASKBAR
        .compare_exchange(previous, light, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let Some(tray) = app.tray_by_id("lumentray") else {
        trace("[tray] theme refresh: tray icon not found");
        TRAY_LIGHT_TASKBAR.store(previous, Ordering::Relaxed);
        return;
    };
    let result = tauri::image::Image::from_bytes(tray_icon_bytes(light))
        .map_err(|error| error.to_string())
        .and_then(|icon| tray.set_icon(Some(icon)).map_err(|error| error.to_string()));
    match result {
        Ok(()) => trace(&format!(
            "[tray] icon switched for {} taskbar",
            if light { "light" } else { "dark" }
        )),
        Err(error) => {
            trace(&format!("[tray] theme refresh failed: {error}"));
            TRAY_LIGHT_TASKBAR.store(previous, Ordering::Relaxed);
        }
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开亮度", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "重新检测显示器", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &settings, &refresh, &quit])?;

    let mut builder = TrayIconBuilder::with_id("lumentray")
        .tooltip("LumenTray — 调节显示器亮度")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_flyout(app),
            "settings" => {
                if let Err(error) = open_settings_window(app) {
                    trace(&format!("[tray] failed to open settings: {error}"));
                }
            }
            "refresh" => {
                if let Some(window) = app.get_webview_window(FLYOUT_LABEL) {
                    let _ = window.emit("refresh-monitors", ());
                }
            }
            "quit" => {
                QUITTING.store(true, Ordering::Relaxed);
                if let Some(store) = app.try_state::<SettingsStore>() {
                    let _ = store.save_now();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                toggle_flyout(tray.app_handle(), position);
            }
        });

    let light_taskbar = system_uses_light_taskbar();
    TRAY_LIGHT_TASKBAR.store(light_taskbar, Ordering::Relaxed);
    match tauri::image::Image::from_bytes(tray_icon_bytes(light_taskbar)) {
        Ok(icon) => builder = builder.icon(icon),
        Err(error) => trace(&format!("[tray] icon decode failed: {error}")),
    }

    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let main_handle = app.clone();
            let worker_handle = app.clone();
            let _ = main_handle.run_on_main_thread(move || {
                show_flyout(&worker_handle);
            });
        }))
        .invoke_handler(tauri::generate_handler![
            list_monitors,
            get_monitor_snapshot,
            set_brightness,
            close_flyout,
            reveal_flyout,
            resize_flyout,
            open_settings,
            reveal_settings,
            close_settings,
            get_settings,
            save_settings,
            osd::get_osd_state,
            hotkeys::set_hotkey_recording,
            diagnostics::get_diagnostics,
            diagnostics::read_log_tail,
            diagnostics::open_log_folder,
            diagnostics::copy_text
        ])
        .setup(|app| {
            let config_directory = settings::config_directory(app.handle());
            logging::init(&config_directory);
            trace(&format!(
                "[app] LumenTray {} starting",
                app.package_info().version
            ));
            let store = SettingsStore::load(app.handle());
            trace(&format!(
                "[settings] {} ({} monitors, hotkeys {})",
                store.settings_path(),
                store.monitor_count(),
                if store.snapshot().hotkeys_enabled {
                    "on"
                } else {
                    "off"
                }
            ));
            let _ = app.manage(store);
            build_tray(app.handle())?;
            spawn_event_window(app.handle().clone());

            let restore_handle = app.handle().clone();
            let _ = std::thread::Builder::new()
                .name("lumentray-restore-startup".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    #[cfg(debug_assertions)]
                    let force = std::env::var_os("LUMENTRAY_RESTORE_NOW").is_some();
                    #[cfg(not(debug_assertions))]
                    let force = false;
                    restore_saved_brightness(&restore_handle, force);
                });

            // 预热显示器缓存，让第一次打开飞窗也有内容可渲染，不用等枚举。
            let warm_handle = app.handle().clone();
            let _ = std::thread::Builder::new()
                .name("lumentray-cache-warm".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(2000));
                    if cache::cached_monitors().is_some() {
                        return;
                    }
                    let started = Instant::now();
                    tauri::async_runtime::block_on(async move {
                        let list = list_monitors(warm_handle).await;
                        trace(&format!(
                            "[perf] cache warm: {} monitors in {}ms",
                            list.monitors.len(),
                            started.elapsed().as_millis()
                        ));
                    });
                });

            #[cfg(debug_assertions)]
            if std::env::var_os("LUMENTRAY_SHOW_ON_START").is_some() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(900));
                    let anchor = handle
                        .primary_monitor()
                        .ok()
                        .flatten()
                        .map(|monitor| {
                            let area = monitor.work_area();
                            PhysicalPosition::new(
                                (area.position.x + area.size.width as i32 - 200) as f64,
                                (area.position.y + area.size.height as i32 - 20) as f64,
                            )
                        })
                        .unwrap_or(PhysicalPosition::new(0.0, 0.0));
                    let main_handle = handle.clone();
                    let worker_handle = main_handle.clone();
                    let _ = main_handle.run_on_main_thread(move || {
                        if let Err(error) = create_flyout(&worker_handle, anchor) {
                            trace(&format!("[dev] create failed: {error}"));
                        }
                    });
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } if !QUITTING.load(Ordering::Relaxed) => {
                api.prevent_exit();
            }
            tauri::RunEvent::Exit => {
                if let Some(store) = app.try_state::<SettingsStore>() {
                    let _ = store.save_now();
                }
            }
            _ => {}
        });
}
