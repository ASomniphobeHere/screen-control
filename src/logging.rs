//! Log to the terminal in console mode, and to a daily log file in service mode.

use std::path::Path;

use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;

/// Log files kept, one per day.
const KEEP_FILES: usize = 30;

/// The log level can be changed with the `RUST_LOG` environment variable.
fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

pub fn init_console() {
    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_writer(std::io::stderr)
        .init();
}

/// Keep the returned guard until the program ends, or the last lines are lost.
pub fn init_file(dir: &Path) -> Result<WorkerGuard> {
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("screen-control")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(dir)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Ok(guard)
}
