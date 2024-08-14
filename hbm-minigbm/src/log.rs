// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use log::{LevelFilter, Log, Metadata, Record};
use std::io::Write;
use std::sync::{Mutex, Once};
use std::{env, fs};

type LoggerCallback = Box<dyn Fn(&Record) + Send>;

struct LoggerState {
    callback: LoggerCallback,
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

    fn update_callback(&self, cb: LoggerCallback) {
        let mut state = self.state.lock().unwrap();
        let state = state.as_mut().unwrap();

        state.callback = cb;
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

pub fn init(max_lv: LevelFilter, cb: LoggerCallback) {
    static ONCE: Once = Once::new();
    ONCE.call_once(init_once);

    log::set_max_level(max_lv);

    LOGGER.update_callback(cb);
}
