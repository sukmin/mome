//! Minimal file logger.
//!
//! Release builds have no console, and the interesting moments (Windows shutdown,
//! restore at the next sign-in) happen when nobody is watching, so log lines go to
//! `%LOCALAPPDATA%\mome\mome.log`. Debug builds also echo them to stderr.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::sync::Mutex;

use crate::paths;
use crate::time::LocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct FileLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // Our own messages at the global level; only warnings from dependencies (wgpu is chatty).
        metadata.target().starts_with("mome") || metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = LocalTime::now();
        let line = format!(
            "[{}.{:03}] [{}] {} {}\n",
            now.display(),
            now.millis,
            std::process::id(),
            record.level(),
            record.args()
        );
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

pub fn init() {
    let path = paths::log_file();
    let file = (|| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        if std::fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
        OpenOptions::new().create(true).append(true).open(&path).ok()
    })();
    let logger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(if cfg!(debug_assertions) {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        });
    }
}
