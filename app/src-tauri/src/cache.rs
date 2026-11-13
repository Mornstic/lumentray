use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use ddc_core::{MonitorInfo, MonitorList};

static BRIGHTNESS: LazyLock<Mutex<HashMap<String, (u32, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MONITORS: LazyLock<Mutex<Option<Vec<MonitorInfo>>>> = LazyLock::new(|| Mutex::new(None));

pub fn note(id: &str, percent: u32) {
    if let Ok(mut map) = BRIGHTNESS.lock() {
        map.insert(id.to_string(), (percent, Instant::now()));
    }
}

pub fn remember(list: &MonitorList) {
    if let Ok(mut map) = BRIGHTNESS.lock() {
        map.retain(|id, _| list.monitors.iter().any(|monitor| &monitor.id == id));
    }
    for monitor in &list.monitors {
        if let Some(brightness) = &monitor.brightness {
            note(&monitor.id, brightness.percent);
        }
    }
    if let Ok(mut snapshot) = MONITORS.lock() {
        *snapshot = Some(list.monitors.clone());
    }
}

pub fn update_monitor(id: &str, percent: u32) {
    if let Ok(mut snapshot) = MONITORS.lock()
        && let Some(monitors) = snapshot.as_mut()
        && let Some(monitor) = monitors.iter_mut().find(|monitor| monitor.id == id)
        && let Some(brightness) = &mut monitor.brightness
    {
        brightness.percent = percent;
        monitor.error = None;
    }
}

pub fn cached_monitors() -> Option<Vec<MonitorInfo>> {
    MONITORS.lock().ok().and_then(|snapshot| snapshot.clone())
}

pub fn brightnesses(max_age: Duration) -> Option<HashMap<String, u32>> {
    let map = BRIGHTNESS.lock().ok()?;
    if map.is_empty() || map.values().any(|(_, at)| at.elapsed() > max_age) {
        return None;
    }
    Some(to_map(&map))
}

pub fn known_brightnesses() -> HashMap<String, u32> {
    BRIGHTNESS
        .lock()
        .map(|map| to_map(&map))
        .unwrap_or_default()
}

fn to_map(map: &HashMap<String, (u32, std::time::Instant)>) -> HashMap<String, u32> {
    map.iter()
        .map(|(id, (percent, _))| (id.clone(), *percent))
        .collect()
}
