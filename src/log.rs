use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use crate::util;

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn init() {
    let dir = util::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("verbaq.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 1_000_000 {
            let _ = std::fs::remove_file(&path);
        }
    }
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    if let Ok(mut guard) = LOG.lock() {
        *guard = file;
    }
}

pub fn line(message: impl AsRef<str>) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let text = format!("[{stamp}] {}\n", message.as_ref());
    if let Ok(mut guard) = LOG.lock() {
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush();
        }
    }
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::log::line(format!($($arg)*))
    };
}
