//! The main loop. It owns the config and state; other threads send it events.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Utc;

use crate::core::commands::{self, Input, Out};
use crate::core::config::{Config, Secrets};
use crate::core::engine::{Action, Engine, Reason};
use crate::core::state::State;
use crate::telegram::api::Update;
use crate::{storage, telegram, win};

/// Time between ticks.
pub const TICK: Duration = Duration::from_secs(20);

/// Time between checks for new sessions. A tick also runs this often while a sign-out is pending.
const POLL: Duration = Duration::from_secs(1);

const MESSAGE_TITLE: &str = "Screen time";

pub enum Event {
    Stop,
    /// Logon, logoff, lock or unlock. Causes a tick at once.
    SessionChanged,
    Telegram(Update),
    /// A request was sent to a parent. Its message ID is needed to edit it later.
    RequestSent {
        request_id: String,
        to: i64,
        message_id: i64,
    },
}

/// Paths of the files in the data folder.
pub struct DataDir(pub PathBuf);

impl DataDir {
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("ScreenTime")
    }

    pub fn config(&self) -> PathBuf {
        self.0.join("config.json")
    }

    pub fn secrets(&self) -> PathBuf {
        self.0.join("secrets.json")
    }

    pub fn state(&self) -> PathBuf {
        self.0.join("state.json")
    }

    pub fn logs(&self) -> PathBuf {
        self.0.join("logs")
    }
}

/// Loads `secrets.json` (if there is one) and `config.json`, and checks the config.
pub fn load_checked(data: &DataDir) -> Result<(Config, Option<Secrets>)> {
    let secrets = if data.secrets().exists() {
        Some(Secrets::load(&data.secrets())?)
    } else {
        tracing::warn!(path = %data.secrets().display(), "no secrets file, Telegram and config updates are off");
        None
    };
    let cfg = Config::load(&data.config())?;
    let errors = cfg.validate(secrets.as_ref().map(|s| s.pc_name.as_str()));
    if !errors.is_empty() {
        bail!("config has errors:\n  {}", errors.join("\n  "));
    }
    Ok((cfg, secrets))
}

/// Runs until `Event::Stop` comes or all senders are gone.
/// `sender` is given to other threads, so they can send events to this loop.
pub fn run(
    data: &DataDir,
    cfg: Config,
    secrets: Option<Secrets>,
    sender: Sender<Event>,
    events: Receiver<Event>,
) -> Result<()> {
    let token = secrets
        .and_then(|s| s.bot_token)
        .filter(|t| !t.trim().is_empty());
    let telegram = match token {
        Some(token) => match telegram::start(token.trim(), sender) {
            Ok(outbox) => Some(outbox),
            Err(e) => {
                tracing::error!(
                    error = format!("{e:#}"),
                    "cannot start Telegram, going on without it"
                );
                None
            }
        },
        None => {
            tracing::warn!("no bot token, Telegram is off");
            None
        }
    };
    let mut app = App {
        state_path: data.state(),
        state: storage::load_state(&data.state()),
        cfg,
        engine: Engine::new(TICK),
        last_clock: win::clock::unbiased_now(),
        telegram,
    };
    tracing::info!(pc = %app.cfg.pc_name, children = app.cfg.children.len(), telegram = app.telegram.is_some(), "running");

    let mut last_tick: Option<Instant> = None;
    let mut known_sessions = Vec::new();
    loop {
        let mut now = match events.recv_timeout(POLL) {
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::SessionChanged) => {
                tracing::info!("session changed");
                true
            }
            // A command can change the time left, so tick at once.
            Ok(Event::Telegram(update)) => app.telegram_update(update),
            Ok(Event::RequestSent {
                request_id,
                to,
                message_id,
            }) => {
                commands::request_sent(&mut app.state, &request_id, to, message_id);
                app.save();
                false
            }
            Err(RecvTimeoutError::Timeout) => false,
        };
        // Session events can be missed, so also check for a new or ended session.
        match win::sessions::active_session_ids() {
            Ok(ids) if ids != known_sessions => {
                tracing::info!(sessions = ?ids, "active sessions changed");
                known_sessions = ids;
                now = true;
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = format!("{e:#}"), "cannot list sessions"),
        }
        let tick_due = last_tick.is_none_or(|t| t.elapsed() >= TICK);
        if now || tick_due || app.engine.has_pending() {
            app.tick();
            last_tick = Some(Instant::now());
        }
    }

    app.save();
    tracing::info!("stopped, state saved");
    Ok(())
}

struct App {
    state_path: PathBuf,
    state: State,
    cfg: Config,
    engine: Engine,
    last_clock: Duration,
    /// `None` when there is no bot token.
    telegram: Option<Sender<Out>>,
}

impl App {
    /// Returns true if the update was a command, so a tick should follow.
    fn telegram_update(&mut self, update: Update) -> bool {
        let now = Utc::now();
        let input = if let Some(m) = &update.message {
            let (Some(from), Some(text)) = (&m.from, &m.text) else {
                return false;
            };
            let age = now - chrono::DateTime::from_timestamp(m.date, 0).unwrap_or(now);
            if age > commands::MAX_MESSAGE_AGE {
                tracing::info!(
                    from = from.id,
                    text,
                    age_s = age.num_seconds(),
                    "old message ignored"
                );
                return false;
            }
            tracing::info!(from = from.id, text, "Telegram message");
            Input::Text {
                from: from.id,
                text,
            }
        } else if let Some(q) = &update.callback_query {
            let data = q.data.as_deref().unwrap_or("");
            tracing::info!(from = q.from.id, data, "Telegram button");
            Input::Button {
                from: q.from.id,
                callback_id: &q.id,
                data,
            }
        } else {
            return false;
        };
        let outs = commands::handle(&self.cfg, &mut self.state, now, input);
        self.dispatch(outs);
        self.save();
        true
    }

    /// Sends Telegram messages, and shows `NotifyPc` messages on the PC.
    fn dispatch(&self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::NotifyPc { child_id, text } => self.notify_pc(&child_id, &text),
                out => match &self.telegram {
                    Some(telegram) => {
                        let _ = telegram.send(out);
                    }
                    None => tracing::warn!(out = ?out, "Telegram is off, message not sent"),
                },
            }
        }
    }

    fn notify_pc(&self, child_id: &str, text: &str) {
        let Some(child) = self.cfg.children.iter().find(|c| c.id == child_id) else {
            return;
        };
        let sessions = win::sessions::active_sessions().unwrap_or_default();
        for s in sessions
            .iter()
            .filter(|s| s.sid.eq_ignore_ascii_case(&child.sid))
        {
            if let Err(e) = win::actions::send_message(s.id, MESSAGE_TITLE, text) {
                tracing::error!(
                    session = s.id,
                    error = format!("{e:#}"),
                    "cannot show message"
                );
            }
        }
    }

    fn tick(&mut self) {
        let now = win::clock::unbiased_now();
        let delta = now.saturating_sub(self.last_clock);
        self.last_clock = now;

        let sessions = match win::sessions::active_sessions() {
            Ok(s) => s,
            Err(e) => {
                // Without sessions nothing can be counted. Try again on the next tick.
                tracing::error!(error = format!("{e:#}"), "cannot list sessions");
                return;
            }
        };
        let actions = self
            .engine
            .tick(&self.cfg, &mut self.state, Utc::now(), delta, &sessions);
        for action in actions {
            self.run_action(action);
        }
        let expired = commands::expire(&self.cfg, &mut self.state, Utc::now());
        self.dispatch(expired);
        self.save();
    }

    fn run_action(&self, action: Action) {
        let name = |id: &str| {
            self.cfg
                .children
                .iter()
                .find(|c| c.id == id)
                .map_or(id.to_string(), |c| c.name.clone())
        };
        let result = match &action {
            Action::Warn {
                session,
                child_id,
                minutes_left,
                reason,
            } => {
                let n = minutes_left;
                let unit = if *n == 1 { "minute" } else { "minutes" };
                let text = match reason {
                    Reason::Budget => format!(
                        "{}, you have {n} {unit} of screen time left today.",
                        name(child_id)
                    ),
                    Reason::Hours => format!(
                        "{}, screen time ends in {n} {unit}: allowed hours are almost over.",
                        name(child_id)
                    ),
                };
                win::actions::send_message(*session, MESSAGE_TITLE, &text)
            }
            Action::TimeUp {
                session,
                child_id,
                reason,
                grace_s,
            } => {
                let why = match reason {
                    Reason::Budget => "your screen time for today is used up",
                    Reason::Hours => "screen time is not allowed at this hour",
                };
                let mut text = format!(
                    "{}, {why}.\n\nYou will be signed out in {grace_s} seconds.",
                    name(child_id),
                );
                let child = self.cfg.children.iter().find(|c| &c.id == child_id);
                let can_ask =
                    self.telegram.is_some() && child.is_some_and(|c| c.telegram_user_id.is_some());
                if *reason == Reason::Budget && can_ask {
                    text += "\n\nTo ask for more time, send /more 30 to the bot in Telegram.";
                }
                win::actions::send_message(*session, MESSAGE_TITLE, &text)
            }
            Action::Logoff { session, .. } => win::actions::logoff(*session),
        };
        match result {
            Ok(()) => tracing::info!(action = ?action, "done"),
            Err(e) => tracing::error!(action = ?action, error = format!("{e:#}"), "failed"),
        }
    }

    fn save(&self) {
        if let Err(e) = storage::save_json(&self.state_path, &self.state) {
            tracing::error!(error = format!("{e:#}"), "cannot save state");
        }
    }
}
