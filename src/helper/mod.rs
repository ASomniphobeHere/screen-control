//! `screen-control helper`: runs in a child's session and shows what the service sends through
//! the pipe. The service starts it in each child's session.
//!
//! - `helper`: one copy per session. Shows the timer and the popups. A thread reads the pipe and
//!   gives each message to the window thread.
//! - `helper --print`: prints each message, for testing. Not limited to one copy.
//! - `helper --show <popup>...`: shows sample popups without the service, to see how they look.
//!   It only draws in the user's own session and changes nothing.

mod draw;
mod popup;
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
    WM_QUIT, WM_TIMER,
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
    } else if let Some(i) = args.iter().position(|a| a == "--show") {
        logging::init_console();
        show_samples(&args[i + 1..])
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

    // The windows first, so the helper only connects when it can show things.
    let ui = Ui::new(user_file("timer.json"))?;

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

    run_windows(ui, &inbox);
    Ok(ExitCode::SUCCESS)
}

/// Sample messages for `helper --show`.
fn sample(name: &str) -> Option<Message> {
    use crate::core::engine::Reason;
    let warn = |minutes_left, reason| Message::Warn {
        minutes_left,
        reason,
    };
    let time_up = |reason, can_ask| Message::TimeUp {
        grace_s: 10,
        reason,
        can_ask,
    };
    let extra = |minutes, by: &str| Message::ExtraTime {
        minutes,
        by: by.into(),
    };
    Some(match name {
        "warn10" => warn(10, Reason::Budget),
        "warn5" => warn(5, Reason::Budget),
        "warn1" => warn(1, Reason::Hours),
        "time-up" => time_up(Reason::Budget, true),
        "time-up-hours" => time_up(Reason::Hours, false),
        "extra" => extra(30, "Mom"),
        "taken" => extra(-15, "Dad"),
        _ => return None,
    })
}

/// `helper --show <popup>...`: shows sample popups 5 seconds apart, so a newer one replaces an
/// older one, and quits 20 seconds after the last.
fn show_samples(names: &[String]) -> Result<ExitCode> {
    let Some(messages) = names.iter().map(|n| sample(n)).collect::<Option<Vec<_>>>() else {
        eprintln!("popups: warn10, warn5, warn1, time-up, time-up-hours, extra, taken");
        return Ok(ExitCode::FAILURE);
    };
    let mut ui = Ui::new(None)?;
    ui.child_name = Some("Jānis".into());
    let (sender, inbox) = mpsc::channel();
    let ui_thread = unsafe { GetCurrentThreadId() };
    let post = move |msg| unsafe { PostThreadMessageW(ui_thread, msg, WPARAM(0), LPARAM(0)) };
    thread::spawn(move || {
        for message in messages {
            if sender.send(message).is_ok() {
                let _ = post(WM_APP_MESSAGES);
            }
            thread::sleep(Duration::from_secs(5));
        }
        thread::sleep(Duration::from_secs(15));
        let _ = post(WM_QUIT);
    });
    run_windows(ui, &inbox);
    Ok(ExitCode::SUCCESS)
}

/// The windows of the helper.
struct Ui {
    timer: timer::Timer,
    popup: popup::Popup,
    /// From the last `status`, for the popups.
    child_name: Option<String>,
}

impl Ui {
    /// Makes the windows, hidden until messages come.
    fn new(timer_prefs: Option<PathBuf>) -> Result<Ui> {
        // Sharp on each screen, at any display scaling.
        if let Err(e) =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        {
            tracing::warn!(error = %e, "cannot set DPI awareness");
        }
        let draw = Rc::new(draw::Draw::new()?);
        Ok(Ui {
            timer: timer::Timer::new(Rc::clone(&draw), timer_prefs)?,
            popup: popup::Popup::new(draw)?,
            child_name: None,
        })
    }

    fn on_message(&mut self, message: Message) {
        let name = self.child_name.clone();
        match message {
            Message::Status {
                child_name,
                left_s,
                limit_s,
                show_timer,
                ..
            } => {
                self.child_name = Some(child_name);
                self.timer.set_status(left_s, limit_s, show_timer);
                self.popup.on_status(left_s);
            }
            Message::Warn {
                minutes_left,
                reason,
            } => {
                self.timer.make_large();
                self.popup.show(popup::Content::Warn {
                    name,
                    minutes_left,
                    reason,
                });
            }
            Message::TimeUp {
                grace_s,
                reason,
                can_ask,
            } => self.popup.show(popup::Content::TimeUp {
                name,
                grace_s,
                reason,
                can_ask,
            }),
            Message::ExtraTime { minutes, by } => {
                self.popup.show(popup::Content::ExtraTime { minutes, by })
            }
        }
    }
}

/// The window thread's message loop. Runs until the process ends, or until `WM_QUIT`.
fn run_windows(mut ui: Ui, inbox: &Receiver<Message>) {
    // A thread timer: WM_TIMER comes with no window.
    unsafe { SetTimer(None, 0, TICK_MS, None) };
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.hwnd.is_invalid() {
            match msg.message {
                WM_APP_MESSAGES => {
                    while let Ok(message) = inbox.try_recv() {
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
