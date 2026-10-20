//! Config file types and checks. The config comes from the private config repo.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Weekday;
use serde::Deserialize;

pub const MINUTES_PER_DAY: u32 = 24 * 60;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub pc_name: String,
    /// Local time = UTC + this offset. The system time zone is not used.
    pub utc_offset_minutes: i32,
    pub parents: Vec<Parent>,
    pub children: Vec<Child>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parent {
    pub name: String,
    pub telegram_user_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Child {
    pub id: String,
    pub name: String,
    /// Windows account SID on this PC.
    pub sid: String,
    #[serde(default)]
    pub telegram_user_id: Option<i64>,
    pub limits: Limits,
    /// No value means use is allowed at any hour.
    #[serde(default)]
    pub allowed_hours: Option<TimeRange>,
}

/// Daily limits in minutes. A day without its own value uses `default`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub default: Option<u32>,
    pub mon: Option<u32>,
    pub tue: Option<u32>,
    pub wed: Option<u32>,
    pub thu: Option<u32>,
    pub fri: Option<u32>,
    pub sat: Option<u32>,
    pub sun: Option<u32>,
}

impl Limits {
    pub fn for_day(&self, day: Weekday) -> Option<u32> {
        let value = match day {
            Weekday::Mon => self.mon,
            Weekday::Tue => self.tue,
            Weekday::Wed => self.wed,
            Weekday::Thu => self.thu,
            Weekday::Fri => self.fri,
            Weekday::Sat => self.sat,
            Weekday::Sun => self.sun,
        };
        value.or(self.default)
    }
}

/// A time range like `08:00-21:00`, in minutes after midnight.
/// When `end` is before `start`, the range goes over midnight (`22:00-02:00`).
/// `24:00` is allowed as the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct TimeRange {
    pub start: u32,
    pub end: u32,
}

impl TryFrom<String> for TimeRange {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let bad = || format!("bad time range \"{text}\", use the form HH:MM-HH:MM");
        let (start, end) = text.split_once('-').ok_or_else(bad)?;
        let start = parse_time(start.trim()).ok_or_else(bad)?;
        let end = parse_time(end.trim()).ok_or_else(bad)?;
        if start == MINUTES_PER_DAY {
            return Err(format!("time range \"{text}\" cannot start at 24:00"));
        }
        if start == end {
            return Err(format!(
                "time range \"{text}\" starts and ends at the same time"
            ));
        }
        Ok(TimeRange { start, end })
    }
}

fn parse_time(text: &str) -> Option<u32> {
    let (h, m) = text.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    match (h, m) {
        (24, 0) => Some(MINUTES_PER_DAY),
        (0..=23, 0..=59) => Some(h * 60 + m),
        _ => None,
    }
}

impl TimeRange {
    pub fn contains(&self, minute: u32) -> bool {
        if self.start < self.end {
            minute >= self.start && minute < self.end
        } else {
            minute >= self.start || minute < self.end
        }
    }

    /// Minutes from `minute` to the end of the range. Only valid when `contains(minute)`.
    pub fn minutes_until_end(&self, minute: u32) -> u32 {
        if minute < self.end {
            self.end - minute
        } else {
            MINUTES_PER_DAY - minute + self.end
        }
    }
}

impl Config {
    pub fn from_json(text: &str) -> Result<Config> {
        Ok(serde_json::from_str(text)?)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Config::from_json(&text).with_context(|| format!("bad config in {}", path.display()))
    }

    pub fn child_by_sid(&self, sid: &str) -> Option<&Child> {
        self.children
            .iter()
            .find(|c| c.sid.eq_ignore_ascii_case(sid))
    }

    /// Returns all problems found. An empty list means the config is good.
    /// `pc_name` is the name of this PC from `secrets.json`, if known.
    pub fn validate(&self, pc_name: Option<&str>) -> Vec<String> {
        let mut errors = Vec::new();

        if self.pc_name.trim().is_empty() {
            errors.push("pc_name is empty".to_string());
        }
        if let Some(expected) = pc_name
            && !self.pc_name.eq_ignore_ascii_case(expected)
        {
            errors.push(format!(
                "pc_name is \"{}\", but this PC is \"{expected}\"",
                self.pc_name
            ));
        }
        if !(-12 * 60..=14 * 60).contains(&self.utc_offset_minutes) {
            errors.push(format!(
                "utc_offset_minutes {} is not between -720 and 840",
                self.utc_offset_minutes
            ));
        }

        if self.parents.is_empty() {
            errors.push("there must be at least one parent".to_string());
        }
        let mut telegram_ids = HashSet::new();
        let mut parent_names = HashSet::new();
        for p in &self.parents {
            if p.name.trim().is_empty() {
                errors.push("a parent has an empty name".to_string());
            } else if !parent_names.insert(p.name.to_lowercase()) {
                errors.push(format!("parent name \"{}\" is used more than once", p.name));
            }
            if !telegram_ids.insert(p.telegram_user_id) {
                errors.push(format!(
                    "Telegram user ID {} is used more than once",
                    p.telegram_user_id
                ));
            }
        }

        let mut ids = HashSet::new();
        let mut names: HashSet<String> = HashSet::new();
        let mut sids = HashSet::new();
        for c in &self.children {
            let who = if c.id.is_empty() { &c.name } else { &c.id };
            if c.id.is_empty() || c.id.contains(char::is_whitespace) {
                errors.push(format!(
                    "child \"{who}\": id must not be empty or have spaces"
                ));
            } else if !ids.insert(c.id.to_lowercase()) {
                errors.push(format!("child id \"{}\" is used more than once", c.id));
            }
            // Names and IDs are both used in `/give <name>`, so they must not clash.
            if c.name.trim().is_empty() {
                errors.push(format!("child \"{who}\": name is empty"));
            } else if !names.insert(c.name.to_lowercase()) {
                errors.push(format!("child name \"{}\" is used more than once", c.name));
            }
            if !c.sid.starts_with("S-1-") {
                errors.push(format!(
                    "child \"{who}\": sid \"{}\" does not start with S-1-",
                    c.sid
                ));
            } else if !sids.insert(c.sid.to_uppercase()) {
                errors.push(format!("SID {} is used more than once", c.sid));
            }
            if let Some(id) = c.telegram_user_id
                && !telegram_ids.insert(id)
            {
                errors.push(format!("Telegram user ID {id} is used more than once"));
            }
            for day in ALL_DAYS {
                match c.limits.for_day(day) {
                    None => errors.push(format!(
                        "child \"{who}\": no limit for {day}, set it or set \"default\""
                    )),
                    Some(m) if m > MINUTES_PER_DAY => errors.push(format!(
                        "child \"{who}\": limit for {day} is {m}, the most is {MINUTES_PER_DAY}"
                    )),
                    Some(_) => {}
                }
            }
        }
        for c in &self.children {
            let id = c.id.to_lowercase();
            let clash = self
                .children
                .iter()
                .find(|o| o.id != c.id && o.name.to_lowercase() == id);
            if let Some(other) = clash {
                errors.push(format!(
                    "child id \"{}\" is the same as the name of child \"{}\"",
                    c.id, other.id
                ));
            }
        }

        errors
    }
}

const ALL_DAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

/// Local secrets. Only on this PC, never in the config repo.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    pub pc_name: String,
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
    /// `owner/repo` of the config repo.
    #[serde(default)]
    pub github_repo: Option<String>,
}

impl Secrets {
    pub fn load(path: &Path) -> Result<Secrets> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("bad secrets in {}", path.display()))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const GOOD: &str = r#"{
        "pc_name": "PC-1",
        "utc_offset_minutes": 180,
        "parents": [
            { "name": "Mom", "telegram_user_id": 111 },
            { "name": "Dad", "telegram_user_id": 333 }
        ],
        "children": [
            {
                "id": "janis",
                "name": "Jānis",
                "sid": "S-1-5-21-1-1001",
                "telegram_user_id": 222,
                "limits": { "default": 60, "sat": 120 },
                "allowed_hours": "08:00-21:00"
            },
            {
                "id": "peteris",
                "name": "Pēteris",
                "sid": "S-1-5-21-1-1002",
                "limits": { "default": 30 }
            }
        ]
    }"#;

    pub fn good() -> Config {
        Config::from_json(GOOD).unwrap()
    }

    fn errors_after(change: impl FnOnce(&mut Config)) -> Vec<String> {
        let mut cfg = good();
        change(&mut cfg);
        cfg.validate(Some("PC-1"))
    }

    #[test]
    fn good_config_has_no_errors() {
        assert_eq!(good().validate(Some("pc-1")), Vec::<String>::new());
    }

    #[test]
    fn example_config_in_repo_is_good() {
        let cfg = Config::from_json(include_str!("../../example-config.json")).unwrap();
        assert_eq!(cfg.validate(Some("PC-1")), Vec::<String>::new());
    }

    #[test]
    fn day_limit_falls_back_to_default() {
        let cfg = good();
        assert_eq!(cfg.children[0].limits.for_day(Weekday::Sat), Some(120));
        assert_eq!(cfg.children[0].limits.for_day(Weekday::Mon), Some(60));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let text = GOOD.replace("\"pc_name\"", "\"pc_nme\"");
        assert!(Config::from_json(&text).is_err());
    }

    #[test]
    fn bad_time_ranges_are_rejected() {
        for bad in [
            "8:00-21:00",
            "08:00",
            "08:60-21:00",
            "25:00-01:00",
            "24:00-08:00",
            "08:00-08:00",
        ] {
            assert!(
                TimeRange::try_from(bad.to_string()).is_err(),
                "{bad} should fail"
            );
        }
        assert!(TimeRange::try_from("00:00-24:00".to_string()).is_ok());
    }

    #[test]
    fn wrong_pc_name() {
        assert_eq!(errors_after(|c| c.pc_name = "PC-2".into()).len(), 1);
    }

    #[test]
    fn bad_offset() {
        assert_eq!(errors_after(|c| c.utc_offset_minutes = 900).len(), 1);
    }

    #[test]
    fn no_parents() {
        assert_eq!(errors_after(|c| c.parents.clear()).len(), 1);
    }

    #[test]
    fn duplicate_telegram_id_between_parent_and_child() {
        assert_eq!(
            errors_after(|c| c.children[1].telegram_user_id = Some(111)).len(),
            1
        );
    }

    #[test]
    fn duplicate_child_id_name_sid_and_telegram_id() {
        let errors = errors_after(|c| {
            let copy = c.children[0].clone();
            c.children.push(copy);
        });
        assert_eq!(errors.len(), 4, "{errors:?}");
    }

    #[test]
    fn child_id_same_as_other_childs_name() {
        let errors = errors_after(|c| c.children[1].id = "jānis".into());
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn bad_sid() {
        assert_eq!(
            errors_after(|c| c.children[0].sid = "janis".into()).len(),
            1
        );
    }

    #[test]
    fn missing_day_limit() {
        let errors = errors_after(|c| {
            c.children[1].limits = Limits {
                mon: Some(10),
                ..Default::default()
            }
        });
        assert_eq!(errors.len(), 6, "{errors:?}");
    }

    #[test]
    fn limit_too_large() {
        assert_eq!(
            errors_after(|c| c.children[0].limits.sat = Some(2000)).len(),
            1
        );
    }

    #[test]
    fn child_lookup_by_sid_ignores_case() {
        assert_eq!(
            good().child_by_sid("s-1-5-21-1-1002").unwrap().id,
            "peteris"
        );
    }
}
