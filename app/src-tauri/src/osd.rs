use std::sync::{LazyLock, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::cache;

const OSD_LABEL: &str = "osd";
const OSD_WIDTH: f64 = 400.0;
const OSD_HEIGHT: f64 = 128.0;
const OSD_TOP_MARGIN: f64 = 16.0;
const AUTO_HIDE: Duration = Duration::from_millis(1600);
const FADE_OUT: Duration = Duration::from_millis(240);
const IDLE_CLOSE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsdState {
    name: String,
    index: usize,
    total: usize,
    percent: Option<u32>,
    primary: bool,
    synced: usize,
    reachable: usize,
}

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    Visible(Instant),
    Fading(Instant),
}

#[derive(Clone, Copy)]
enum Command {
    Show,
}

static STATE: LazyLock<Mutex<Option<OsdState>>> = LazyLock::new(|| Mutex::new(None));
static WORKER: OnceLock<mpsc::Sender<Command>> = OnceLock::new();

#[tauri::command]
pub fn get_osd_state() -> Option<OsdState> {
    STATE.lock().ok().and_then(|slot| slot.as_ref().cloned())
}

/// 快捷键调完亮度后调用：取鼠标所在显示器，带上本次同步结果。
pub fn show_after_hotkey(app: &AppHandle, synced: usize, reachable: usize) {
    let monitors = cache::cached_monitors().unwrap_or_default();
    let cursor = app.cursor_position().ok();
    let hit = cursor.and_then(|point| {
        monitors.iter().find(|monitor| {
            let right = monitor.x + monitor.width as i32;
            let bottom = monitor.y + monitor.height as i32;
            point.x >= monitor.x as f64
                && point.x < right as f64
                && point.y >= monitor.y as f64
                && point.y < bottom as f64
        })
    });
    let Some(monitor) = hit
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
        .or_else(|| monitors.first())
    else {
        return;
    };

    let mut ordered: Vec<&ddc_core::MonitorInfo> = monitors.iter().collect();
    ordered.sort_by_key(|monitor| (monitor.x, monitor.y));
    let index = ordered
        .iter()
        .position(|candidate| candidate.id == monitor.id)
        .map(|position| position + 1)
        .unwrap_or(1);

    show(
        app,
        OsdState {
            name: display_name(monitor),
            index,
            total: monitors.len(),
            percent: cache::known_brightnesses().get(&monitor.id).copied(),
            primary: monitor.primary,
            synced,
            reachable,
        },
    );
}

fn display_name(monitor: &ddc_core::MonitorInfo) -> String {
    if !monitor.name.is_empty() {
        return monitor.name.clone();
    }
    if !monitor.description.is_empty() {
        return monitor.description.clone();
    }
    monitor.gdi_name.clone()
}

fn show(app: &AppHandle, state: OsdState) {
    if let Ok(mut slot) = STATE.lock() {
        *slot = Some(state);
    }
    let sender = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        let worker_app = app.clone();
        let _ = thread::Builder::new()
            .name("lumentray-osd".into())
            .spawn(move || worker(worker_app, receiver));
        sender
    });
    let _ = sender.send(Command::Show);
}

fn worker(app: AppHandle, receiver: mpsc::Receiver<Command>) {
    let mut phase = Phase::Idle;
    let mut close_at: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let timeout = match phase {
            Phase::Visible(deadline) | Phase::Fading(deadline) => {
                deadline.saturating_duration_since(now)
            }
            Phase::Idle => close_at
                .map(|deadline| deadline.saturating_duration_since(now))
                .unwrap_or(Duration::from_secs(3600)),
        };
        match receiver.recv_timeout(timeout) {
            Ok(Command::Show) => {
                let now = Instant::now();
                phase = Phase::Visible(now + visible_duration());
                close_at = Some(now + IDLE_CLOSE);
                present(&app);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match phase {
                Phase::Visible(deadline) if Instant::now() >= deadline => {
                    let _ = app.emit("osd-hide", ());
                    phase = Phase::Fading(Instant::now() + FADE_OUT);
                }
                Phase::Fading(deadline) if Instant::now() >= deadline => {
                    if let Some(window) = app.get_webview_window(OSD_LABEL) {
                        let _ = hide_window(&window);
                    }
                    phase = Phase::Idle;
                }
                Phase::Idle => {
                    if let Some(deadline) = close_at
                        && Instant::now() >= deadline
                    {
                        close_at = None;
                        if let Some(window) = app.get_webview_window(OSD_LABEL) {
                            let _ = window.close();
                        }
                    }
                }
                _ => {}
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn present(app: &AppHandle) {
    let state = STATE.lock().ok().and_then(|slot| slot.as_ref().cloned());
    let Some(state) = state else {
        return;
    };
    let main_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let window = match main_app.get_webview_window(OSD_LABEL) {
            Some(window) => Ok(window),
            None => create_window(&main_app),
        };
        match window {
            Ok(window) => {
                if let Err(error) = place(&window) {
                    crate::trace(&format!("[osd] place failed: {error}"));
                }
                // Tauri 的 show() 会走 SW_SHOW 抢焦点，OSD 必须用 SW_SHOWNOACTIVATE。
                if let Err(error) = show_window(&window) {
                    crate::trace(&format!("[osd] show failed: {error}"));
                }
                let _ = window.emit("osd-state", &state);
            }
            Err(error) => crate::trace(&format!("[osd] create failed: {error}")),
        }
    });
}

fn create_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let started = Instant::now();
    let window = WebviewWindowBuilder::new(
        app,
        OSD_LABEL,
        WebviewUrl::App("index.html?view=osd".into()),
    )
    .title("LumenTray OSD")
    .inner_size(OSD_WIDTH, OSD_HEIGHT)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .focused(false)
    .focusable(false)
    .build()?;
    let _ = window.set_ignore_cursor_events(true);
    if let Err(error) = prepare_window(&window) {
        crate::trace(&format!("[osd] style setup failed: {error}"));
    }
    crate::trace(&format!(
        "[perf] osd window built {}ms",
        started.elapsed().as_millis()
    ));
    Ok(window)
}

/// tao 默认给无父窗口加 WS_EX_APPWINDOW（会进任务栏与 Alt+Tab），
/// 且创建后会清掉“不聚焦”标记，show() 退化成会抢焦点的 SW_SHOW。
fn prepare_window(window: &WebviewWindow) -> tauri::Result<()> {
    let hwnd = window.hwnd()?;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let style = (style & !(WS_EX_APPWINDOW.0 as isize))
            | WS_EX_TOOLWINDOW.0 as isize
            | WS_EX_NOACTIVATE.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    Ok(())
}

fn show_window(window: &WebviewWindow) -> tauri::Result<()> {
    let hwnd = window.hwnd()?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

fn hide_window(window: &WebviewWindow) -> tauri::Result<()> {
    let hwnd = window.hwnd()?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    Ok(())
}

fn place(window: &WebviewWindow) -> tauri::Result<()> {
    let app = window.app_handle();
    let cursor = app.cursor_position().ok();
    let monitor = cursor
        .and_then(|point| app.monitor_from_point(point.x, point.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return Ok(());
    };

    let scale = monitor.scale_factor();
    let width = (OSD_WIDTH * scale).round() as i32;
    let height = (OSD_HEIGHT * scale).round() as i32;
    let area = monitor.work_area();
    let x = area.position.x + (area.size.width as i32 - width) / 2;
    let y = area.position.y + (OSD_TOP_MARGIN * scale).round() as i32;

    window.set_size(PhysicalSize::new(width as u32, height as u32))?;
    window.set_position(PhysicalPosition::new(x, y))?;
    Ok(())
}

fn visible_duration() -> Duration {
    #[cfg(debug_assertions)]
    if std::env::var_os("LUMENTRAY_OSD_HOLD").is_some() {
        return Duration::from_secs(600);
    }
    AUTO_HIDE
}
