//! `screen-control helper`: runs in a child's session and shows what the service sends through
//! the pipe. In this version only `helper --print` works: it prints each message, for testing.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::windows::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

use crate::helper_protocol::{Message, PIPE_NAME};

/// Time between tries to connect when the pipe is not there or busy.
const RETRY: Duration = Duration::from_secs(2);

pub fn run(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "--print") {
        print_messages()
    } else {
        bail!("only `helper --print` works in this version")
    }
}

/// Prints each message line with the local time. Connects again when the pipe closes,
/// for example when the service restarts. Runs until stopped with Ctrl+C.
fn print_messages() -> Result<ExitCode> {
    loop {
        let pipe = connect();
        eprintln!("connected to {PIPE_NAME}");
        for line in BufReader::new(pipe).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    eprintln!("pipe closed: {e}");
                    break;
                }
            };
            let time = chrono::Local::now().format("%H:%M:%S");
            match Message::from_line(&line) {
                Ok(_) => println!("{time} {line}"),
                Err(e) => println!("{time} BAD {e:#}"),
            }
        }
        eprintln!("disconnected, connecting again");
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
                    eprintln!("cannot connect, trying again: {text}");
                    last_error = text;
                }
                thread::sleep(RETRY);
            }
        }
    }
}
