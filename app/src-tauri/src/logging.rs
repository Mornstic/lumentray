use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const KEEP_BACKUPS: usize = 3;
const RING_LINES: usize = 400;

struct Logger {
    path: PathBuf,
    ring: Mutex<VecDeque<String>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init(directory: &Path) -> PathBuf {
    let logs = directory.join("logs");
    let path = logs.join("lumentray.log");
    if LOGGER.get().is_none() {
        if let Err(error) = fs::create_dir_all(&logs) {
            eprintln!("[logging] create {} failed: {error}", logs.display());
        }
        rotate_if_needed(&path);
        let _ = LOGGER.set(Logger {
            path: path.clone(),
            ring: Mutex::new(VecDeque::with_capacity(RING_LINES)),
        });
    }
    path
}

pub fn path() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.path.clone())
}

pub fn write(message: &str) {
    let line = format!("{} {message}", timestamp());
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut ring) = logger.ring.lock() {
            ring.push_back(line.clone());
            if ring.len() > RING_LINES {
                ring.pop_front();
            }
        }
        append(&logger.path, &line);
    }
    #[cfg(debug_assertions)]
    eprintln!("{line}");
}

pub fn tail(lines: usize) -> Vec<String> {
    let lines = lines.clamp(1, 500);
    let Some(path) = path() else {
        return Vec::new();
    };
    match fs::read_to_string(&path) {
        Ok(text) => text
            .lines()
            .rev()
            .take(lines)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(str::to_string)
            .collect(),
        Err(_) => LOGGER
            .get()
            .and_then(|logger| {
                logger
                    .ring
                    .lock()
                    .ok()
                    .map(|ring| ring.iter().rev().take(lines).rev().cloned().collect())
            })
            .unwrap_or_default(),
    }
}

fn append(path: &Path, line: &str) {
    if let Ok(metadata) = fs::metadata(path)
        && metadata.len() >= MAX_LOG_BYTES
    {
        rotate(path);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn rotate_if_needed(path: &Path) {
    if let Ok(metadata) = fs::metadata(path)
        && metadata.len() >= MAX_LOG_BYTES
    {
        rotate(path);
    }
}

fn rotate(path: &Path) {
    for index in (1..=KEEP_BACKUPS).rev() {
        let target = backup_path(path, index);
        let _ = fs::remove_file(&target);
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            backup_path(path, index - 1)
        };
        if source.exists() {
            let _ = fs::rename(&source, &target);
        }
    }
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    path.with_file_name(format!("lumentray.{index}.log"))
}

fn timestamp() -> String {
    let time = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_shifts_backups_and_drops_the_oldest() {
        let directory =
            std::env::temp_dir().join(format!("lumentray-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("lumentray.log");
        fs::write(&path, "current").unwrap();
        fs::write(backup_path(&path, 1), "one").unwrap();
        fs::write(backup_path(&path, 2), "two").unwrap();
        fs::write(backup_path(&path, 3), "three").unwrap();

        rotate(&path);

        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(backup_path(&path, 1)).unwrap(),
            "current"
        );
        assert_eq!(fs::read_to_string(backup_path(&path, 2)).unwrap(), "one");
        assert_eq!(fs::read_to_string(backup_path(&path, 3)).unwrap(), "two");
        let _ = fs::remove_dir_all(&directory);
    }
}
