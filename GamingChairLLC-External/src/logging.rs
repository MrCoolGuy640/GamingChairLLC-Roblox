//! Logging: console + rotating file, with the same line format as the Python app.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Local;
use log::{LevelFilter, Log, Metadata, Record};

use crate::config::{self, Paths};

const MAX_LOG_BYTES: u64 = 2_000_000;
const BACKUPS: usize = 3;

pub fn init(paths: &Paths) -> Result<()> {
    fs::create_dir_all(&paths.log_dir)
        .with_context(|| format!("creating log directory {}", paths.log_dir.display()))?;

    let level = if config::DEBUG {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    let logger = AppLogger {
        level,
        file: Mutex::new(LogFile::new(paths.log_file.clone())),
    };

    log::set_boxed_logger(Box::new(logger)).map_err(|err| anyhow::anyhow!("{err}"))?;
    log::set_max_level(level);

    // A panic in any thread must end up in the log file, not only the console.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(target: "GamingChairLLC", "UNHANDLED PANIC: {info}");
        default_hook(info);
    }));

    Ok(())
}

struct AppLogger {
    level: LevelFilter,
    file: Mutex<LogFile>,
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let current = std::thread::current();
        let thread = current.name().unwrap_or("unnamed");
        let line = format!(
            "{timestamp} | {:<8} | {thread} | {} | {}\n",
            record.level(),
            record.target(),
            record.args()
        );

        print!("{line}");
        let _ = std::io::stdout().flush();

        if let Ok(mut file) = self.file.lock() {
            file.write(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            file.flush();
        }
    }
}

struct LogFile {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl LogFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            size: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.file.is_none() {
            self.open();
        }

        if self.size.saturating_add(bytes.len() as u64) > MAX_LOG_BYTES {
            self.rotate();
        }

        if let Some(file) = self.file.as_mut() {
            if file.write_all(bytes).is_ok() {
                self.size = self.size.saturating_add(bytes.len() as u64);
                let _ = file.flush();
            }
        }
    }

    fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }

    fn open(&mut self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => {
                self.size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                self.file = Some(file);
            }
            Err(err) => {
                eprintln!("Could not open log file {}: {err}", self.path.display());
                self.file = None;
                self.size = 0;
            }
        }
    }

    fn rotate(&mut self) {
        self.file.take();

        for index in (1..=BACKUPS).rev() {
            let from = backup_path(&self.path, index);
            let to = backup_path(&self.path, index + 1);
            if index == BACKUPS {
                let _ = fs::remove_file(&to);
            }
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }

        let _ = fs::rename(&self.path, backup_path(&self.path, 1));
        self.size = 0;
        self.open();
    }
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{index}", path.display()))
}
