//! `screen-control helper`: runs in a child's session and shows what the service sends through
//! the pipe. The service starts it in each child's session.
//!
//! - `helper`: one copy per session. In this version it only reads the messages and logs them;
//!   the windows come in steps 4 and 5 of the better warnings plan.
//! - `helper --print`: prints each message, for testing. Not limited to one copy.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

use crate::helper_protocol::{Message, PIPE_NAME};
use crate::logging;
use crate::win::single_copy::SingleCopy;

/// Time between tries to connect when the pipe is not there or busy.
const RETRY: Duration = Duration::from_secs(2);

/// Only one helper per session. `Local\` names are per session.
const MUTEX_NAME: &str = r"Local\ScreenControlHelper";

pub fn run(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "--print") {
        logging::init_console();
        read_messages(|line, message| {
            let time = chrono::Local::now().format("%H:%M:%S");
            match message {
                Ok(_) => println!("{time} {line}"),
                Err(e) => println!("{time} BAD {e:#}"),
            }
        })
    } else {
        run_helper()
    }
}

/// The helper the service starts. It has no console, so it logs to a small file of its own.
fn run_helper() -> Result<ExitCode> {
    if let Some(path) = log_path()
        && let Err(e) = logging::init_helper(&path)
    {
        // Nowhere to report it. Go on without a log.
        eprintln!("cannot open the log {}: {e:#}", path.display());
    }
    let Some(_single) = SingleCopy::claim(MUTEX_NAME)? else {
        tracing::info!("a helper already runs in this session, quitting");
        return Ok(ExitCode::SUCCESS);
    };
    tracing::info!(pid = std::process::id(), "helper started");
    let mut first = true;
    read_messages(|line, message| match message {
        // Only the first status after a connect, so the log stays small.
        Ok(message) if first => {
            tracing::info!(?message, "first message");
            first = false;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(line, error = format!("{e:#}"), "bad message"),
    })
}

/// `%LOCALAPPDATA%\ScreenControl\helper.log` of the user the helper runs as.
fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("ScreenControl").join("helper.log"))
}

/// Reads the messages and gives each line to `on_message`. Connects again when the pipe closes,
/// for example when the service restarts. Runs until the process is stopped; when the session
/// ends, Windows ends it.
fn read_messages(mut on_message: impl FnMut(&str, Result<Message>)) -> Result<ExitCode> {
    loop {
        let pipe = connect();
        tracing::info!(pipe = PIPE_NAME, "connected");
        for line in BufReader::new(pipe).lines() {
            match line {
                Ok(line) => on_message(&line, Message::from_line(&line)),
                Err(e) => {
                    tracing::info!(error = %e, "pipe closed");
                    break;
                }
            }
        }
        tracing::info!("disconnected, connecting again");
        thread::sleep(RETRY);
    }
}

/// Waits until the pipe can be opened.
fn connect() -> File {
    let mut last_error = String::new();
    loop {
        // Read only. Identification level: a program that pretends to be the service
        // cannot act as this user.
        match OpenOptions::new()
            .read(true)
            .security_qos_flags(SECURITY_IDENTIFICATION.0)
            .open(PIPE_NAME)
        {
            Ok(pipe) => return pipe,
            Err(e) => {
                let text = e.to_string();
                if text != last_error {
                    tracing::info!(error = %text, "cannot connect, trying again");
                    last_error = text;
                }
                thread::sleep(RETRY);
            }
        }
    }
}
