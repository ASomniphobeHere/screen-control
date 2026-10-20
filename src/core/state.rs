//! Saved state: used time, extra time and open requests.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The local date that `children` belongs to.
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub last_seen_utc: Option<DateTime<Utc>>,
    /// SHA of the config file in use, from the config repo.
    #[serde(default)]
    pub config_sha: Option<String>,
    /// By child ID.
    #[serde(default)]
    pub children: BTreeMap<String, ChildState>,
    #[serde(default)]
    pub requests: Vec<Request>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChildState {
    #[serde(default)]
    pub used_ms: u64,
    /// Given by parents today. Can be below zero after `/take`.
    #[serde(default)]
    pub extra_ms: i64,
    /// Warning thresholds (in minutes) already shown.
    #[serde(default)]
    pub warned: BTreeSet<u32>,
}

/// An extra time request from a child, waiting for a parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub child_id: String,
    pub minutes: u32,
    pub created_utc: DateTime<Utc>,
    /// The copy of the request sent to each parent.
    #[serde(default)]
    pub parent_messages: Vec<ParentMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentMessage {
    pub telegram_user_id: i64,
    pub message_id: i64,
}

impl State {
    /// Starts a new day if `date` is not the saved date. Returns true if it did.
    /// Requests are kept: they expire on their own.
    pub fn roll_day(&mut self, date: NaiveDate) -> bool {
        if self.date == Some(date) {
            return false;
        }
        self.date = Some(date);
        self.children.clear();
        true
    }

    pub fn child_mut(&mut self, child_id: &str) -> &mut ChildState {
        self.children.entry(child_id.to_string()).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn roll_day_resets_children_only_on_new_date() {
        let mut state = State::default();
        assert!(state.roll_day(date(20)));
        state.child_mut("janis").used_ms = 5000;
        state.child_mut("janis").extra_ms = 60_000;
        state.child_mut("janis").warned.insert(10);

        assert!(!state.roll_day(date(20)));
        assert_eq!(state.child_mut("janis").used_ms, 5000);

        assert!(state.roll_day(date(21)));
        assert_eq!(state.child_mut("janis"), &ChildState::default());
    }

    #[test]
    fn roll_day_keeps_requests() {
        let mut state = State::default();
        state.roll_day(date(20));
        state.requests.push(Request {
            id: "a1".into(),
            child_id: "janis".into(),
            minutes: 30,
            created_utc: Utc::now(),
            parent_messages: vec![],
        });
        state.roll_day(date(21));
        assert_eq!(state.requests.len(), 1);
    }

    #[test]
    fn old_state_file_without_new_fields_loads() {
        let state: State = serde_json::from_str(r#"{ "date": "2026-10-20" }"#).unwrap();
        assert_eq!(state.date, Some(date(20)));
        assert!(state.children.is_empty());
    }
}
