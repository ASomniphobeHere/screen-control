//! `screen-control helper`: runs in a child's session and shows what the service sends through
//! the pipe. The service starts it in each child's session.
//!
//! - `helper`: one copy per session. Shows the timer. A thread reads the pipe and gives each
//!   message to the window thread. The warning windows come in step 5 of the better warnings
//!   plan; until then those messages are only logged.
//! - `helper --print`: prints each message, for testing. Not limited to one copy.

mod draw;
mod timer;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SetTimer, TranslateMessage, WM_APP,
    WM_TIMER,
};

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

/// Posted to the window thread when messages are waiting.
const WM_APP_MESSAGES: u32 = WM_APP + 100;

/// The timer counts down this often.
const TICK_MS: u32 = 1000;

/// The helper the service starts. It has no console, so it logs to a small file of its own.
fn run_helper() -> Result<ExitCode> {
    if let Some(path) = user_file("helper.log")
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

    // Sharp on each screen, at any display scaling.
    if let Err(e) =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
    {
        tracing::warn!(error = %e, "cannot set DPI awareness");
    }
    // The windows first, so the helper only connects when it can show things.
    let draw = Rc::new(draw::Draw::new()?);
    let timer = timer::Timer::new(draw, user_file("timer.json"))?;

    let (sender, inbox) = mpsc::channel();
    let ui_thread = unsafe { GetCurrentThreadId() };
    thread::spawn(move || {
        let mut first = true;
        read_messages(|line, message| match message {
            Ok(message) => {
                // Only the first status after a connect, so the log stays small.
                if first || !matches!(message, Message::Status { .. }) {
                    tracing::info!(?message, "message");
                    first = false;
                }
                if sender.send(message).is_ok() {
                    let _ = unsafe {
                        PostThreadMessageW(ui_thread, WM_APP_MESSAGES, WPARAM(0), LPARAM(0))
                    };
                }
            }
            Err(e) => tracing::warn!(line, error = format!("{e:#}"), "bad message"),
        })
    });

    let mut ui = Ui { timer, inbox };
    run_windows(&mut ui);
    Ok(ExitCode::SUCCESS)
}

/// The windows of the helper and the messages waiting for them.
struct Ui {
    timer: timer::Timer,
    inbox: Receiver<Message>,
}

impl Ui {
    fn on_message(&mut self, message: Message) {
        match message {
            Message::Status {
                left_s,
                limit_s,
                show_timer,
                ..
            } => self.timer.set_status(left_s, limit_s, show_timer),
            // Step 5 shows the warning window here.
            Message::Warn { .. } => self.timer.make_large(),
            Message::TimeUp { .. } | Message::ExtraTime { .. } => {}
        }
    }
}

/// The window thread's message loop. Runs until the process ends.
fn run_windows(ui: &mut Ui) {
    // A thread timer: WM_TIMER comes with no window.
    unsafe { SetTimer(None, 0, TICK_MS, None) };
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.hwnd.is_invalid() {
            match msg.message {
                WM_APP_MESSAGES => {
                    while let Ok(message) = ui.inbox.try_recv() {
                        ui.on_message(message);
                    }
                }
                WM_TIMER => ui.timer.tick(),
                _ => {}
            }
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// A file in `%LOCALAPPDATA%\ScreenControl` of the user the helper runs as.
fn user_file(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("ScreenControl").join(name))
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
