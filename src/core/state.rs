//! Saved state: used time, extra time, passes and open requests.

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
    /// Extra time after hours, by child ID. Not reset at a new day.
    #[serde(default)]
    pub passes: BTreeMap<String, Pass>,
    #[serde(default)]
    pub requests: Vec<Request>,
    /// Makes request IDs. Not reset at a new day.
    #[serde(default)]
    pub next_request_id: u32,
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

/// Extra time given when the allowed hours end before it would run out. It runs on the clock.
/// While it is active, the budget and the allowed hours do not apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pass {
    pub start_utc: DateTime<Utc>,
    pub end_utc: DateTime<Utc>,
}

impl Pass {
    /// Active while start <= now < end. So a clock put back before the start ends it too.
    pub fn is_active(&self, utc: DateTime<Utc>) -> bool {
        self.start_utc <= utc && utc < self.end_utc
    }
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

    pub fn active_pass(&self, child_id: &str, utc: DateTime<Utc>) -> Option<&Pass> {
        self.passes.get(child_id).filter(|p| p.is_active(utc))
    }

    /// Removes passes that are not active. Returns the IDs of their children.
    pub fn drop_ended_passes(&mut self, utc: DateTime<Utc>) -> Vec<String> {
        let ended: Vec<String> = self
            .passes
            .iter()
            .filter(|(_, p)| !p.is_active(utc))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ended {
            self.passes.remove(id);
        }
        ended
    }

    pub fn child_mut(&mut self, child_id: &str) -> &mut ChildState {
        self.children.entry(child_id.to_string()).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

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
        assert!(state.passes.is_empty());
    }

    fn pass(start_min: i64, end_min: i64) -> Pass {
        let base = Utc.with_ymd_and_hms(2026, 10, 20, 18, 0, 0).unwrap();
        Pass {
            start_utc: base + TimeDelta::minutes(start_min),
            end_utc: base + TimeDelta::minutes(end_min),
        }
    }

    #[test]
    fn pass_is_active_from_start_to_end() {
        let p = pass(0, 30);
        let at = |m| p.start_utc + TimeDelta::minutes(m);
        assert!(!p.is_active(at(-1)));
        assert!(p.is_active(at(0)));
        assert!(p.is_active(at(29)));
        assert!(!p.is_active(at(30)));
    }

    #[test]
    fn roll_day_keeps_passes_and_ended_ones_are_dropped() {
        let mut state = State::default();
        state.roll_day(date(20));
        state.passes.insert("janis".into(), pass(0, 30));
        state.passes.insert("peteris".into(), pass(0, 90));
        state.roll_day(date(21));
        assert_eq!(state.passes.len(), 2);

        let now = pass(45, 45).start_utc;
        assert!(state.active_pass("janis", now).is_none());
        assert!(state.active_pass("peteris", now).is_some());
        assert_eq!(state.drop_ended_passes(now), ["janis"]);
        assert_eq!(state.passes.keys().collect::<Vec<_>>(), ["peteris"]);
    }
}
