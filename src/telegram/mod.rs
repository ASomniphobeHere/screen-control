//! Telegram: one thread reads new messages, one thread sends. The main loop never waits for the network.

pub mod api;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use self::api::Api;
use crate::app::Event;
use crate::core::commands::Out;

const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SEND_TRIES: u32 = 3;

/// Starts both threads. Send `Out` messages to the returned sender.
/// `Out::NotifyPc` and `Out::Reload` are not Telegram messages and must not be sent here.
pub fn start(token: &str, events: Sender<Event>) -> Result<Sender<Out>> {
    let api = Arc::new(Api::new(token)?);
    let (outbox, rx) = mpsc::channel();

    let (poll_api, poll_events) = (api.clone(), events.clone());
    thread::Builder::new()
        .name("telegram-poll".into())
        .spawn(move || poll(&poll_api, &poll_events))
        .context("cannot start the Telegram thread")?;
    thread::Builder::new()
        .name("telegram-send".into())
        .spawn(move || send_all(&api, &rx, &events))
        .context("cannot start the Telegram thread")?;
    Ok(outbox)
}

fn poll(api: &Api, events: &Sender<Event>) {
    let mut offset = 0;
    let mut backoff = Duration::from_secs(5);
    loop {
        match api.get_updates(offset) {
            Ok(updates) => {
                backoff = Duration::from_secs(5);
                for update in updates {
                    offset = update.update_id + 1;
                    if events.send(Event::Telegram(update)).is_err() {
                        return; // The main loop has ended.
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    retry_s = backoff.as_secs(),
                    "cannot get Telegram messages"
                );
                thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

fn send_all(api: &Api, rx: &Receiver<Out>, events: &Sender<Event>) {
    for out in rx {
        for attempt in 1..=SEND_TRIES {
            match send_one(api, &out, events) {
                Ok(()) => break,
                Err(e) if attempt < SEND_TRIES => {
                    tracing::warn!(
                        error = format!("{e:#}"),
                        attempt,
                        "Telegram send failed, trying again"
                    );
                    thread::sleep(Duration::from_secs(5));
                }
                Err(e) => {
                    tracing::error!(error = format!("{e:#}"), out = ?out, "Telegram send failed, giving up")
                }
            }
        }
    }
}

fn send_one(api: &Api, out: &Out, events: &Sender<Event>) -> Result<()> {
    match out {
        Out::Send {
            to,
            text,
            buttons,
            request_id,
        } => {
            let message_id = api.send_message(*to, text, buttons)?;
            if let Some(request_id) = request_id {
                let _ = events.send(Event::RequestSent {
                    request_id: request_id.clone(),
                    to: *to,
                    message_id,
                });
            }
        }
        Out::Edit {
            to,
            message_id,
            text,
        } => api.edit_message_text(*to, *message_id, text)?,
        Out::Answer { callback_id, text } => api.answer_callback_query(callback_id, text)?,
        Out::NotifyPc { .. } | Out::Reload { .. } => {
            tracing::error!(out = ?out, "not a Telegram message, ignored")
        }
    }
    Ok(())
}
