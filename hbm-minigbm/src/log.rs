// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use log::{LevelFilter, Log, Metadata, Record};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::{env, fs, io, process, sync};

struct Logger {
    syslog: Option<syslog::BasicLogger>,
}

impl Log for Logger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        eprintln!(
            "hbm: {}: {}: {}",
            record.level(),
            record.target(),
            record.args()
        );

        if let Some(syslog) = &self.syslog {
            syslog.log(record);
        }
    }

    fn flush(&self) {
        if let Some(syslog) = &self.syslog {
            syslog.flush();
        }
    }
}

fn get_max_level() -> LevelFilter {
    if env::var("HBM_DEBUG").is_ok() {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    }
}

fn is_stderr_null() -> bool {
    let stderr_md = {
        let fd = io::stderr().as_raw_fd();
        // SAFETY: fd is valid
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let md = file.metadata();
        file.into_raw_fd();

        md
    };

    if stderr_md.is_err() {
        return true;
    }

    let null_md = Path::new("/dev/null").metadata();
    if null_md.is_err() {
        return true;
    }

    stderr_md.unwrap().rdev() == null_md.unwrap().rdev()
}

fn init_once() {
    let syslog = if is_stderr_null() {
        let formatter = syslog::Formatter3164 {
            facility: syslog::Facility::LOG_USER,
            hostname: None,
            process: String::from("hbm"),
            pid: process::id(),
        };
        syslog::unix(formatter).map(syslog::BasicLogger::new).ok()
    } else {
        None
    };

    let logger = Logger { syslog };

    let _ = log::set_boxed_logger(Box::new(logger)).map(|_| log::set_max_level(get_max_level()));
}

pub fn init() {
    static ONCE: sync::Once = sync::Once::new();
    ONCE.call_once(init_once);
}
