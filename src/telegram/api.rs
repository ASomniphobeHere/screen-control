//! A small client for the Telegram Bot API: only the calls this program needs.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::core::commands::Button;

/// How long `getUpdates` waits on Telegram's side for new messages.
pub const POLL_TIMEOUT_S: u64 = 50;

pub struct Api {
    client: reqwest::blocking::Client,
    /// Contains the token. Never log it.
    base: String,
}

#[derive(Deserialize)]
struct Response<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    /// Unix time in seconds.
    pub date: i64,
    pub from: Option<User>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub data: Option<String>,
}

impl Api {
    pub fn new(token: &str) -> Result<Api> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(POLL_TIMEOUT_S + 20))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .context("cannot make the HTTP client")?;
        Ok(Api {
            client,
            base: format!("https://api.telegram.org/bot{token}"),
        })
    }

    fn call<T: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<T> {
        // `without_url` keeps the token out of error messages and logs.
        let response: Response<T> = self
            .client
            .post(format!("{}/{method}", self.base))
            .json(body)
            .send()
            .map_err(|e| e.without_url())
            .with_context(|| format!("Telegram {method} failed"))?
            .json()
            .map_err(|e| e.without_url())
            .with_context(|| format!("Telegram {method}: bad answer"))?;
        if !response.ok {
            bail!(
                "Telegram {method}: {}",
                response.description.unwrap_or_default()
            );
        }
        response
            .result
            .with_context(|| format!("Telegram {method}: no result"))
    }

    pub fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        self.call(
            "getUpdates",
            &json!({
                "offset": offset,
                "timeout": POLL_TIMEOUT_S,
                "allowed_updates": ["message", "callback_query"],
            }),
        )
    }

    /// Returns the ID of the new message.
    pub fn send_message(&self, chat_id: i64, text: &str, buttons: &[Button]) -> Result<i64> {
        let mut body = json!({ "chat_id": chat_id, "text": text });
        if !buttons.is_empty() {
            let row: Vec<Value> = buttons
                .iter()
                .map(|b| json!({ "text": b.text, "callback_data": b.data }))
                .collect();
            body["reply_markup"] = json!({ "inline_keyboard": [row] });
        }
        let message: Message = self.call("sendMessage", &body)?;
        Ok(message.message_id)
    }

    /// Replaces the text and removes the buttons.
    pub fn edit_message_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let _: Value = self.call(
            "editMessageText",
            &json!({ "chat_id": chat_id, "message_id": message_id, "text": text }),
        )?;
        Ok(())
    }

    pub fn answer_callback_query(&self, callback_id: &str, text: &str) -> Result<()> {
        let _: bool = self.call(
            "answerCallbackQuery",
            &json!({ "callback_query_id": callback_id, "text": text }),
        )?;
        Ok(())
    }
}
