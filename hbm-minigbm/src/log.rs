// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use log::{LevelFilter, Log, Metadata, Record};
use std::io::Write;
use std::sync::{Mutex, Once};
use std::{env, fmt, fs};

type LoggerCallback = Box<dyn Fn(&Record) + Send>;

struct LoggerState {
    callback: Option<LoggerCallback>,
    file: Option<fs::File>,
}

struct Logger {
    state: Mutex<LoggerState>,
}

impl Logger {
    fn init(&self) {
        let mut state = self.state.lock().unwrap();

        state.callback = Some(Self::nop_callback());

        if let Ok(filename) = env::var("HBM_LOG_FILE") {
            state.file = fs::File::create(filename).ok();
        }
    }

    fn update_callback(&self, cb: LoggerCallback) {
        let mut state = self.state.lock().unwrap();

        state.callback = Some(cb);
    }

    fn nop_callback() -> LoggerCallback {
        let cb = |_rec: &Record| {};
        Box::new(cb)
    }
}

impl Log for Logger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, rec: &Record) {
        let mut state = self.state.lock().unwrap();

        (state.callback.as_ref().unwrap())(rec);

        if let Some(file) = state.file.as_mut() {
            let _ = writeln!(file, "{}: {}", rec.level(), rec.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger {
    state: Mutex::new(LoggerState {
        callback: None,
        file: None,
    }),
};

fn init_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        LOGGER.init();
        let _ = log::set_logger(&LOGGER);
    });
}

pub fn enable(max_lv: LevelFilter, cb: LoggerCallback) {
    init_once();
    log::set_max_level(max_lv);
    LOGGER.update_callback(cb);
}

pub fn disable() {
    init_once();
    log::set_max_level(log::LevelFilter::Off);
    LOGGER.update_callback(Logger::nop_callback());
}

// helper trait to log Result::Err
pub trait LogError {
    fn log_err<D>(self, act: D) -> Self
    where
        D: fmt::Display;
}

impl<T> LogError for hbm::Result<T> {
    fn log_err<D>(self, act: D) -> Self
    where
        D: fmt::Display,
    {
        if let Err(err) = &self {
            log::error!("failed to {act}: {err}");
        }

        self
    }
}

#[test]
fn test_level_enabled() {
    use std::sync::Arc;
    let cb_call_count = Arc::new(Mutex::new(0));

    let captured_count = cb_call_count.clone();
    let cb = Box::new(move |rec: &Record| {
        let mut count = captured_count.lock().unwrap();
        *count += 1;
        println!("{}", rec.args());
    });
    enable(LevelFilter::Off, cb);

    log::error!("This shouldn't reach anybody");
    assert!(*cb_call_count.lock().unwrap() == 0);

    log::set_max_level(LevelFilter::Info);
    log::info!("This should be heard");
    assert!(*cb_call_count.lock().unwrap() == 1);

    log::set_max_level(LevelFilter::Warn);
    log::info!("This shouldn't be heard");
    assert!(*cb_call_count.lock().unwrap() == 1);

    log::set_max_level(LevelFilter::Debug);
    log::info!("But this should");
    assert!(*cb_call_count.lock().unwrap() == 2);

    disable();
    log::error!("obviously this shouldn't be heard");
    assert!(*cb_call_count.lock().unwrap() == 2);
}
