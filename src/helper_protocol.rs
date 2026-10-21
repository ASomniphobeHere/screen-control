//! Messages from the service to the helper program, sent through the named pipe.
//! Each message is one line of JSON. Pure Rust, so both sides can share and test it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::engine::Reason;

/// The pipe the service makes and the helper reads.
pub const PIPE_NAME: &str = r"\\.\pipe\ScreenControl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Sent every tick, and when the helper connects.
    Status {
        child_name: String,
        /// Seconds left today. Can be below zero.
        left_s: i64,
        /// Today's limit in seconds, with extra time.
        limit_s: u64,
        /// What ends the time first.
        reason: Reason,
        show_timer: bool,
    },
    /// A few minutes are left.
    Warn { minutes_left: u32, reason: Reason },
    /// Time is up, sign-out comes after `grace_s` seconds.
    TimeUp {
        grace_s: u64,
        reason: Reason,
        /// The child can ask for more time with `/more` in Telegram.
        can_ask: bool,
    },
    /// A parent gave time, or took it when `minutes` is below zero.
    ExtraTime { minutes: i32, by: String },
}

impl Message {
    /// The `type` field, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Message::Status { .. } => "status",
            Message::Warn { .. } => "warn",
            Message::TimeUp { .. } => "time_up",
            Message::ExtraTime { .. } => "extra_time",
        }
    }

    /// One JSON line, ending with `\n`. JSON strings escape new lines, so the line has no others.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("messages always convert to JSON");
        line.push('\n');
        line
    }

    /// Reads one line written by `to_line`. The line end is optional.
    pub fn from_line(line: &str) -> Result<Message> {
        let text = line.trim_end_matches(['\r', '\n']);
        serde_json::from_str(text).with_context(|| format!("bad helper message: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the message, checks the exact line, and reads it back.
    fn check(msg: Message, json: &str) {
        let line = msg.to_line();
        assert_eq!(line, format!("{json}\n"));
        assert!(line.starts_with(&format!(r#"{{"type":"{}""#, msg.kind())));
        assert_eq!(Message::from_line(&line).unwrap(), msg);
    }

    #[test]
    fn status() {
        check(
            Message::Status {
                child_name: "Jānis".into(),
                left_s: 5100,
                limit_s: 7200,
                reason: Reason::Budget,
                show_timer: true,
            },
            r#"{"type":"status","child_name":"Jānis","left_s":5100,"limit_s":7200,"reason":"budget","show_timer":true}"#,
        );
    }

    #[test]
    fn status_with_time_below_zero() {
        check(
            Message::Status {
                child_name: "Pēteris".into(),
                left_s: -30,
                limit_s: 0,
                reason: Reason::Hours,
                show_timer: false,
            },
            r#"{"type":"status","child_name":"Pēteris","left_s":-30,"limit_s":0,"reason":"hours","show_timer":false}"#,
        );
    }

    #[test]
    fn warn() {
        check(
            Message::Warn {
                minutes_left: 5,
                reason: Reason::Hours,
            },
            r#"{"type":"warn","minutes_left":5,"reason":"hours"}"#,
        );
    }

    #[test]
    fn time_up() {
        check(
            Message::TimeUp {
                grace_s: 10,
                reason: Reason::Budget,
                can_ask: true,
            },
            r#"{"type":"time_up","grace_s":10,"reason":"budget","can_ask":true}"#,
        );
    }

    #[test]
    fn extra_time_given_and_taken() {
        check(
            Message::ExtraTime {
                minutes: 30,
                by: "Mom".into(),
            },
            r#"{"type":"extra_time","minutes":30,"by":"Mom"}"#,
        );
        check(
            Message::ExtraTime {
                minutes: -15,
                by: "Dad".into(),
            },
            r#"{"type":"extra_time","minutes":-15,"by":"Dad"}"#,
        );
    }

    #[test]
    fn new_line_in_text_stays_on_one_line() {
        let line = Message::ExtraTime {
            minutes: 1,
            by: "a\nb".into(),
        }
        .to_line();
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn windows_line_end_is_accepted() {
        let msg =
            Message::from_line("{\"type\":\"warn\",\"minutes_left\":1,\"reason\":\"budget\"}\r\n");
        assert_eq!(
            msg.unwrap(),
            Message::Warn {
                minutes_left: 1,
                reason: Reason::Budget
            }
        );
    }

    #[test]
    fn bad_lines_are_rejected() {
        for bad in [
            "",
            "not json",
            r#"{"type":"unknown"}"#,
            r#"{"type":"warn","minutes_left":1,"reason":"lunch"}"#,
            r#"{"type":"warn","minutes_left":1}"#,
        ] {
            assert!(Message::from_line(bad).is_err(), "{bad} should fail");
        }
    }
}
