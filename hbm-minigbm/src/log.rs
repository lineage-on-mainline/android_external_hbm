// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::capi::*;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::io::Write;
use std::sync::{Mutex, Once};
use std::{env, ffi, fs};

struct LoggerState {
    callback: Box<dyn Fn(&Record) + Send>,
    file: Option<fs::File>,
}

struct Logger {
    state: Mutex<Option<LoggerState>>,
}

impl Logger {
    fn init(&self) {
        let mut state = self.state.lock().unwrap();

        let callback = Box::new(|_rec: &Record| {});

        let mut file = None;
        if let Ok(filename) = env::var("HBM_LOG_FILE") {
            file = fs::File::create(filename).ok();
        }

        *state = Some(LoggerState { callback, file });
    }

    fn update_callback<T>(&self, cb: T)
    where
        T: Fn(&Record) + Send + 'static,
    {
        let mut state = self.state.lock().unwrap();
        let state = state.as_mut().unwrap();

        state.callback = Box::new(cb);
    }
}

impl Log for Logger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, rec: &Record) {
        let mut state = self.state.lock().unwrap();
        let state = state.as_mut().unwrap();

        (state.callback)(rec);

        if let Some(file) = state.file.as_mut() {
            let _ = writeln!(file, "{}: {}", rec.level(), rec.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger {
    state: Mutex::new(None),
};

fn init_once() {
    LOGGER.init();
    let _ = log::set_logger(&LOGGER);
}

struct CLogger {
    logger: hbm_logger,
    data: *mut ffi::c_void,
}

impl CLogger {
    fn log(&self, rec: &Record) {
        let lv = match rec.level() {
            Level::Error => HBM_LOG_ERROR,
            Level::Warn => HBM_LOG_WARN,
            Level::Info => HBM_LOG_INFO,
            Level::Debug => HBM_LOG_DEBUG,
            Level::Trace => HBM_LOG_DEBUG,
        };

        let msg = format!("{}", rec.args());

        if let Ok(c_msg) = ffi::CString::new(msg) {
            // SAFETY: logger is a valid function pointer
            unsafe {
                (self.logger)(lv, c_msg.as_ptr(), self.data);
            }
        }
    }
}

// SAFETY: users should provide the guarantees
unsafe impl Send for CLogger {}

fn set_max_level(lv: i32) {
    let filter = match lv {
        HBM_LOG_ERROR => LevelFilter::Error,
        HBM_LOG_WARN => LevelFilter::Warn,
        HBM_LOG_INFO => LevelFilter::Info,
        HBM_LOG_DEBUG => LevelFilter::Debug,
        _ => LevelFilter::Error,
    };

    log::set_max_level(filter);
}

pub fn init(max_lv: i32, logger: hbm_logger, data: *mut ffi::c_void) {
    static ONCE: Once = Once::new();
    ONCE.call_once(init_once);

    set_max_level(max_lv);

    let c_logger = CLogger { logger, data };
    LOGGER.update_callback(move |rec: &Record| {
        c_logger.log(rec);
    });
}
