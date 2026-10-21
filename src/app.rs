//! The main loop. It owns the config and state; other threads send it events.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::config_sync::{self, Repo, SyncResult};
use crate::core::commands::{self, Input, Out};
use crate::core::config::{Config, Secrets};
use crate::core::day;
use crate::core::engine::{self, Action, Engine, Reason, Session};
use crate::core::state::State;
use crate::helper_protocol::Message;
use crate::telegram::api::Update;
use crate::win::pipe_server::{self, Helpers};
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
    /// A helper connected to the pipe from this session.
    HelperConnected {
        session: u32,
    },
    /// A check of the config repo. `requested_by` is the parent who sent /reload.
    ConfigSync {
        result: SyncResult,
        requested_by: Option<i64>,
    },
}

/// The first 7 characters of a SHA, as GitHub shows them.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
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

    /// A copy of the last config that passed the checks.
    pub fn config_backup(&self) -> PathBuf {
        self.0.join("config.last-good.json")
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

/// Loads `secrets.json` (if there is one) and the config, and checks the config.
///
/// Without a good config nothing is limited, so the config is taken from the first of these that works:
/// `config.json`, the config repo, then `config.last-good.json`.
pub fn load_checked(data: &DataDir) -> Result<(Config, Option<Secrets>)> {
    let secrets = if data.secrets().exists() {
        Some(Secrets::load(&data.secrets())?)
    } else {
        tracing::warn!(path = %data.secrets().display(), "no secrets file, Telegram and config updates are off");
        None
    };
    let pc_name = secrets.as_ref().map(|s| s.pc_name.as_str());
    let check = |text: String| -> Result<(Config, String)> {
        let cfg = Config::from_json(&text).and_then(|cfg| checked(cfg, pc_name))?;
        Ok((cfg, text))
    };
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))
    };
    let mut errors = Vec::new();

    match read(data.config()).and_then(check) {
        Ok((cfg, text)) => {
            save_backup(data, &text);
            return Ok((cfg, secrets));
        }
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "config.json cannot be used");
            errors.push(format!("config.json: {e:#}"));
        }
    }

    if let Some(repo) = secrets.as_ref().and_then(config_repo) {
        match repo.fetch().and_then(|(_, text)| check(text)) {
            Ok((cfg, text)) => {
                storage::save_text(&data.config(), &text)?;
                save_backup(data, &text);
                tracing::info!("config downloaded from the config repo");
                return Ok((cfg, secrets));
            }
            Err(e) => {
                tracing::error!(
                    error = format!("{e:#}"),
                    "cannot get the config from the config repo"
                );
                errors.push(format!("config repo: {e:#}"));
            }
        }
    }

    match read(data.config_backup()).and_then(check) {
        Ok((cfg, _)) => {
            tracing::warn!("using the last good config");
            Ok((cfg, secrets))
        }
        Err(e) => {
            errors.push(format!("config.last-good.json: {e:#}"));
            bail!("no config can be used:\n{}", errors.join("\n"))
        }
    }
}

/// Keeps a copy of a config that passed the checks, for when no other config can be used.
fn save_backup(data: &DataDir, text: &str) {
    if let Err(e) = storage::save_text(&data.config_backup(), text) {
        tracing::error!(error = format!("{e:#}"), "cannot save the backup config");
    }
}

/// Returns the config if it has no errors.
fn checked(cfg: Config, pc_name: Option<&str>) -> Result<Config> {
    let errors = cfg.validate(pc_name);
    if !errors.is_empty() {
        bail!("config has errors:\n  {}", errors.join("\n  "));
    }
    Ok(cfg)
}

/// The config repo, if `secrets.json` has a GitHub token and repo.
fn config_repo(secrets: &Secrets) -> Option<Repo> {
    let token = secrets
        .github_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    let repo = secrets
        .github_repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())?;
    Repo::new(token, repo, &secrets.pc_name)
        .map_err(|e| tracing::error!(error = format!("{e:#}"), "config updates are off"))
        .ok()
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
    let pc_name = secrets.as_ref().map(|s| s.pc_name.clone());
    let repo = secrets.as_ref().and_then(config_repo);
    if repo.is_none() {
        tracing::warn!("no GitHub token or config repo, config updates are off");
    }
    let token = secrets
        .and_then(|s| s.bot_token)
        .filter(|t| !t.trim().is_empty());
    let telegram = match token {
        Some(token) => match telegram::start(token.trim(), sender.clone()) {
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
    let helpers = pipe_server::start(sender.clone())
        .map_err(|e| tracing::error!(error = format!("{e:#}"), "going on without helpers"))
        .ok();
    let state = storage::load_state(&data.state());
    let sync = repo.and_then(|repo| {
        config_sync::start(repo, state.config_sha.clone(), sender)
            .map_err(|e| tracing::error!(error = format!("{e:#}"), "config updates are off"))
            .ok()
    });
    let mut app = App {
        state_path: data.state(),
        config_path: data.config(),
        backup_path: data.config_backup(),
        state,
        cfg,
        pc_name,
        engine: Engine::new(TICK),
        last_clock: win::clock::unbiased_now(),
        telegram,
        sync,
        helpers,
    };
    tracing::info!(
        pc = %app.cfg.pc_name,
        children = app.cfg.children.len(),
        telegram = app.telegram.is_some(),
        config_updates = app.sync.is_some(),
        helpers = app.helpers.is_some(),
        "running"
    );

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
            Ok(Event::ConfigSync {
                result,
                requested_by,
            }) => app.config_sync(result, requested_by),
            Ok(Event::HelperConnected { session }) => {
                match win::sessions::active_sessions() {
                    Ok(sessions) => app.send_status(sessions.iter().filter(|s| s.id == session)),
                    Err(e) => tracing::error!(error = format!("{e:#}"), "cannot list sessions"),
                }
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
    config_path: PathBuf,
    backup_path: PathBuf,
    state: State,
    cfg: Config,
    /// From `secrets.json`. A new config must have this PC name.
    pc_name: Option<String>,
    engine: Engine,
    last_clock: Duration,
    /// `None` when there is no bot token.
    telegram: Option<Sender<Out>>,
    /// `None` when there is no GitHub token or repo. Send a parent's ID to check now.
    sync: Option<Sender<i64>>,
    /// `None` when the pipe cannot be made.
    helpers: Option<Helpers>,
}

impl App {
    /// Returns true if a new config is used, so a tick should follow.
    fn config_sync(&mut self, result: SyncResult, requested_by: Option<i64>) -> bool {
        let pc = self.cfg.pc_name.clone();
        let reply = |app: &Self, text: String| {
            if let Some(to) = requested_by {
                app.telegram_send(Out::Send {
                    to,
                    text,
                    buttons: Vec::new(),
                    request_id: None,
                });
            }
        };
        let (sha, commit, text) = match result {
            SyncResult::Unchanged { sha } => {
                reply(
                    self,
                    format!("{pc}: the config is up to date ({}).", short(&sha)),
                );
                return false;
            }
            SyncResult::Failed(e) => {
                reply(self, format!("{pc}: cannot get the config. {e}"));
                return false;
            }
            SyncResult::Changed { sha, commit, text } => (sha, commit, text),
        };
        let label = short(commit.as_deref().unwrap_or(&sha));

        let cfg = match Config::from_json(&text).and_then(|c| checked(c, self.pc_name.as_deref())) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!(sha = %sha, error = format!("{e:#}"), "new config has errors, not used");
                self.notify_parents(format!(
                    "{pc}: the new config ({label}) has errors and was not used. The old config stays.\n\n{e:#}"
                ));
                return false;
            }
        };
        let old_text = std::fs::read_to_string(&self.config_path).unwrap_or_default();
        if let Err(e) = storage::save_text(&self.config_path, &text) {
            tracing::error!(error = format!("{e:#}"), "cannot save the new config");
            return false;
        }
        if let Err(e) = storage::save_text(&self.backup_path, &text) {
            tracing::error!(error = format!("{e:#}"), "cannot save the backup config");
        }
        let old_sha = self.state.config_sha.replace(sha.clone());
        self.save();
        self.cfg = cfg;

        let changes = config_sync::diff(&old_text, &text);
        if changes.is_empty() {
            tracing::info!(sha = %sha, "config is up to date");
            reply(self, format!("{pc}: the config is up to date ({label})."));
            return false;
        }
        tracing::info!(old_sha = ?old_sha, new_sha = %sha, changes = ?changes, "config updated");
        let mut message = format!("{pc}: config updated ({label}).");
        for line in changes.iter().take(10) {
            message += &format!("\n• {line}");
        }
        if changes.len() > 10 {
            message += &format!("\n…and {} more changes", changes.len() - 10);
        }
        self.notify_parents(message);
        true
    }

    fn telegram_send(&self, out: Out) {
        match &self.telegram {
            Some(telegram) => {
                let _ = telegram.send(out);
            }
            None => tracing::warn!(out = ?out, "Telegram is off, message not sent"),
        }
    }

    fn notify_parents(&self, text: String) {
        for p in &self.cfg.parents {
            self.telegram_send(Out::Send {
                to: p.telegram_user_id,
                text: text.clone(),
                buttons: Vec::new(),
                request_id: None,
            });
        }
    }

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
                Out::Reload { to } => {
                    let text = match &self.sync {
                        Some(sync) if sync.send(to).is_ok() => "Checking for a new config…",
                        _ => "Config updates are not set up on this PC (no GitHub token or repo).",
                    };
                    self.telegram_send(Out::Send {
                        to,
                        text: text.to_string(),
                        buttons: Vec::new(),
                        request_id: None,
                    });
                }
                out => self.telegram_send(out),
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
        self.send_status(sessions.iter());
    }

    /// Sends the time left to the helper of each child's session.
    fn send_status<'a>(&self, sessions: impl Iterator<Item = &'a Session>) {
        let Some(helpers) = &self.helpers else {
            return;
        };
        let utc = day::effective_utc(Utc::now(), self.state.last_seen_utc);
        let now = day::local_now(utc, self.cfg.utc_offset_minutes);
        for s in sessions {
            let Some(child) = self.cfg.child_by_sid(&s.sid) else {
                continue;
            };
            // Before the first tick of a new day, the saved time is from yesterday.
            let cs = match self.state.children.get(&child.id) {
                Some(cs) if self.state.date == Some(now.date) => cs.clone(),
                _ => Default::default(),
            };
            let t = engine::time_left(child, &cs, &now);
            helpers.send(
                s.id,
                Message::Status {
                    child_name: child.name.clone(),
                    left_s: t.left_ms / 1000,
                    limit_s: (t.limit_ms.max(0) / 1000) as u64,
                    reason: t.reason,
                    show_timer: child.show_timer,
                },
            );
        }
    }

    /// Sends a message to the helper in the session, if there is one.
    fn send_helper(&self, session: u32, message: Message) {
        if let Some(helpers) = &self.helpers {
            helpers.send(session, message);
        }
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
                // Also sent to the helper, for testing. Step 6 of the better warnings plan
                // sends it only to the helper when one is connected.
                self.send_helper(
                    *session,
                    Message::Warn {
                        minutes_left: *n,
                        reason: *reason,
                    },
                );
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
                let can_ask = *reason == Reason::Budget && can_ask;
                if can_ask {
                    text += "\n\nTo ask for more time, send /more 30 to the bot in Telegram.";
                }
                self.send_helper(
                    *session,
                    Message::TimeUp {
                        grace_s: *grace_s,
                        reason: *reason,
                        can_ask,
                    },
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
