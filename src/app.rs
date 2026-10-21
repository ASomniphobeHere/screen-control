//! The main loop. It owns the config and state; other threads send it events.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Utc;

use crate::core::config::{Config, Secrets};
use crate::core::engine::{Action, Engine, Reason};
use crate::core::state::State;
use crate::storage;
use crate::win;

/// Time between ticks.
pub const TICK: Duration = Duration::from_secs(20);

/// Time between checks for new sessions. A tick also runs this often while a sign-out is pending.
const POLL: Duration = Duration::from_secs(1);

const MESSAGE_TITLE: &str = "Screen time";

pub enum Event {
    Stop,
    /// Logon, logoff, lock or unlock. Causes a tick at once.
    SessionChanged,
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
pub fn run(data: &DataDir, cfg: Config, events: Receiver<Event>) -> Result<()> {
    let mut app = App {
        state_path: data.state(),
        state: storage::load_state(&data.state()),
        cfg,
        engine: Engine::new(TICK),
        last_clock: win::clock::unbiased_now(),
    };
    tracing::info!(pc = %app.cfg.pc_name, children = app.cfg.children.len(), "running");

    let mut last_tick: Option<Instant> = None;
    let mut known_sessions = Vec::new();
    loop {
        let mut now = match events.recv_timeout(POLL) {
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::SessionChanged) => {
                tracing::info!("session changed");
                true
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
}

impl App {
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
                let text = format!(
                    "{}, {why}.\n\nYou will be signed out in {grace_s} seconds.",
                    name(child_id),
                );
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
