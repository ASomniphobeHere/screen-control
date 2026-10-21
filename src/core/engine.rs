//! One tick of the rules: sessions in, actions out. No Windows calls here.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use chrono::Weekday;

use super::config::{Child, Config};
use super::day::{self, LocalNow};
use super::state::{ChildState, State};

/// Warnings are shown when this many minutes are left.
pub const WARN_MINUTES: [u32; 3] = [10, 5, 1];

/// Time between "time is up" and sign-out, so the child can see the message.
pub const GRACE: Duration = Duration::from_secs(10);

/// Shorter grace for a session that starts when time is already up.
/// The child got the warnings before, so there is nothing to save.
pub const LOGON_GRACE: Duration = Duration::from_secs(5);

/// If a session is still there this long after a sign-out, sign it out again.
const LOGOFF_RETRY: Duration = Duration::from_secs(10);

/// Daily limit plus extra time minus used time. Can be below zero.
pub fn budget_left_ms(child: &Child, cs: &ChildState, weekday: Weekday) -> i64 {
    // A missing limit cannot pass the config checks. If it happens anyway, allow nothing.
    let limit_ms = child.limits.for_day(weekday).unwrap_or(0) as i64 * 60_000;
    limit_ms + cs.extra_ms - cs.used_ms as i64
}

/// What `time_left` gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeLeft {
    /// Until the budget is used up or the allowed hours end, whichever comes first. Can be below zero.
    pub left_ms: i64,
    /// Today's limit plus extra time. Can be below zero after `/take`.
    pub limit_ms: i64,
    /// What ends the time first.
    pub reason: Reason,
}

/// The time left now, from the budget and the allowed hours.
pub fn time_left(child: &Child, cs: &ChildState, now: &LocalNow) -> TimeLeft {
    let budget_left = budget_left_ms(child, cs, now.weekday);
    let hours_left = child.allowed_hours.map(|range| {
        if range.contains(now.minute) {
            range.minutes_until_end(now.minute) as i64 * 60_000 - now.second as i64 * 1000
        } else {
            0
        }
    });
    let (left_ms, reason) = match hours_left {
        Some(h) if h < budget_left => (h, Reason::Hours),
        _ => (budget_left, Reason::Budget),
    };
    let limit_ms = child.limits.for_day(now.weekday).unwrap_or(0) as i64 * 60_000 + cs.extra_ms;
    TimeLeft {
        left_ms,
        limit_ms,
        reason,
    }
}

/// Time until `left_ms` reaches the next warning point or zero. A tick at that moment shows the
/// warning or the time-up at once, not up to one tick later. `None` when the time is already up.
pub fn until_next_point(left_ms: i64) -> Option<Duration> {
    if left_ms <= 0 {
        return None;
    }
    // A point is passed when the time left is at or below it, so the next one is below.
    let next = WARN_MINUTES
        .into_iter()
        .map(|m| m as i64 * 60_000)
        .filter(|&p| p < left_ms)
        .max()
        .unwrap_or(0);
    Some(Duration::from_millis((left_ms - next) as u64))
}

/// An active Windows session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: u32,
    pub sid: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The daily limit is used up.
    Budget,
    /// Outside the allowed hours.
    Hours,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Warn {
        session: u32,
        child_id: String,
        minutes_left: u32,
        reason: Reason,
    },
    /// Tell the child that time is up and sign-out comes soon.
    TimeUp {
        session: u32,
        child_id: String,
        reason: Reason,
        /// Seconds until sign-out.
        grace_s: u64,
    },
    Logoff {
        session: u32,
        child_id: String,
        reason: Reason,
    },
}

/// A session that got `TimeUp` and waits for sign-out.
#[derive(Debug)]
struct Pending {
    waited: Duration,
    grace: Duration,
    /// `waited` when the last sign-out was sent.
    logoff_sent: Option<Duration>,
}

pub struct Engine {
    max_delta: Duration,
    /// By session ID. Not saved: after a restart, the child gets the message again.
    pending: HashMap<u32, Pending>,
    /// Session IDs from the last tick, to find new sessions.
    seen: HashSet<u32>,
}

impl Engine {
    pub fn new(tick_interval: Duration) -> Self {
        Engine {
            max_delta: tick_interval * 2,
            pending: HashMap::new(),
            seen: HashSet::new(),
        }
    }

    /// True while a session waits for sign-out. The main loop then ticks more often.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn tick(
        &mut self,
        cfg: &Config,
        state: &mut State,
        now_utc: DateTime<Utc>,
        delta: Duration,
        sessions: &[Session],
    ) -> Vec<Action> {
        let mut actions = Vec::new();

        if delta > self.max_delta {
            tracing::warn!(
                delta_s = delta.as_secs_f64(),
                max_s = self.max_delta.as_secs_f64(),
                "time step too long, capped"
            );
        }
        let delta = delta.min(self.max_delta);
        let delta_ms = delta.as_millis() as u64;

        let utc = day::effective_utc(now_utc, state.last_seen_utc);
        if utc != now_utc {
            tracing::warn!(clock = %now_utc, using = %utc, "clock went back, using last seen time");
        } else if matches!(state.last_seen_utc, Some(last) if now_utc < last) {
            tracing::warn!(clock = %now_utc, last_seen = ?state.last_seen_utc, "clock went far back, taken as a clock fix");
        }
        state.last_seen_utc = Some(utc);

        let now = day::local_now(utc, cfg.utc_offset_minutes);
        if state.roll_day(now.date) {
            tracing::info!(date = %now.date, "new day, used time reset");
        }

        self.pending
            .retain(|id, _| sessions.iter().any(|s| s.id == *id));
        let seen = std::mem::replace(&mut self.seen, sessions.iter().map(|s| s.id).collect());

        for s in sessions {
            let Some(child) = cfg.child_by_sid(&s.sid) else {
                tracing::debug!(session = s.id, sid = %s.sid, "not a child, not controlled");
                continue;
            };
            if s.locked {
                tracing::info!(session = s.id, child = %child.id, "locked, not counted");
                continue;
            }

            let cs = state.child_mut(&child.id);
            // A new session started after the last tick, so the time before it was not used.
            let counted_ms = if seen.contains(&s.id) { delta_ms } else { 0 };
            cs.used_ms += counted_ms;

            let TimeLeft {
                left_ms, reason, ..
            } = time_left(child, cs, &now);

            tracing::info!(
                session = s.id,
                child = %child.id,
                counted_ms,
                used_s = cs.used_ms / 1000,
                extra_s = cs.extra_ms / 1000,
                left_s = left_ms / 1000,
                reason = ?reason,
                "counted"
            );

            if left_ms <= 0 {
                match self.pending.get_mut(&s.id) {
                    None => {
                        let grace = if seen.contains(&s.id) {
                            GRACE
                        } else {
                            LOGON_GRACE
                        };
                        self.pending.insert(
                            s.id,
                            Pending {
                                waited: Duration::ZERO,
                                grace,
                                logoff_sent: None,
                            },
                        );
                        actions.push(Action::TimeUp {
                            session: s.id,
                            child_id: child.id.clone(),
                            reason,
                            grace_s: grace.as_secs(),
                        });
                    }
                    Some(p) => {
                        p.waited += delta;
                        let due = match p.logoff_sent {
                            None => p.waited >= p.grace,
                            // Sent again if the session is still there, in case a sign-out failed.
                            Some(sent) => p.waited - sent >= LOGOFF_RETRY,
                        };
                        if due {
                            p.logoff_sent = Some(p.waited);
                            actions.push(Action::Logoff {
                                session: s.id,
                                child_id: child.id.clone(),
                                reason,
                            });
                        }
                    }
                }
                continue;
            }

            // Time was added during the grace time.
            if self.pending.remove(&s.id).is_some() {
                tracing::info!(session = s.id, child = %child.id, "time added, sign-out cancelled");
            }

            // Warn again for thresholds that are passed again after extra time.
            cs.warned.retain(|&t| left_ms <= t as i64 * 60_000);
            let crossed: Vec<u32> = WARN_MINUTES
                .into_iter()
                .filter(|&t| left_ms <= t as i64 * 60_000 && !cs.warned.contains(&t))
                .collect();
            if !crossed.is_empty() {
                cs.warned.extend(crossed);
                let minutes_left = (left_ms as u64).div_ceil(60_000) as u32;
                actions.push(Action::Warn {
                    session: s.id,
                    child_id: child.id.clone(),
                    minutes_left,
                    reason,
                });
            }
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::tests::good;
    use chrono::TimeZone;

    const TICK: Duration = Duration::from_secs(20);
    const JANIS: &str = "S-1-5-21-1-1001"; // 60 min, 120 on Saturday, 08:00-21:00
    const PETERIS: &str = "S-1-5-21-1-1002"; // 30 min, any hour

    /// Tuesday 2026-10-20, local time (UTC+3).
    fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, h, m, s).unwrap() - chrono::TimeDelta::hours(3)
    }

    fn session(id: u32, sid: &str) -> Session {
        Session {
            id,
            sid: sid.into(),
            locked: false,
        }
    }

    struct Test {
        cfg: Config,
        state: State,
        engine: Engine,
        now: DateTime<Utc>,
    }

    impl Test {
        fn new() -> Self {
            Test {
                cfg: good(),
                state: State::default(),
                engine: Engine::new(TICK),
                now: at(12, 0, 0),
            }
        }

        fn tick(&mut self, sessions: &[Session]) -> Vec<Action> {
            self.tick_by(TICK, sessions)
        }

        fn tick_by(&mut self, delta: Duration, sessions: &[Session]) -> Vec<Action> {
            self.now += chrono::TimeDelta::from_std(delta).unwrap();
            self.engine
                .tick(&self.cfg, &mut self.state, self.now, delta, sessions)
        }

        fn used_s(&mut self, child: &str) -> u64 {
            self.state.child_mut(child).used_ms / 1000
        }
    }

    #[test]
    fn counts_only_unlocked_sessions_of_children() {
        let mut t = Test::new();
        let locked = Session {
            locked: true,
            ..session(2, PETERIS)
        };
        let s = [session(1, JANIS), locked, session(3, "S-1-5-21-1-500")];
        t.tick(&s);
        t.tick(&s);
        assert_eq!(t.state.children.keys().collect::<Vec<_>>(), ["janis"]);
        assert_eq!(t.used_s("janis"), 20);
    }

    #[test]
    fn first_tick_of_a_new_session_counts_nothing() {
        let mut t = Test::new();
        t.tick(&[]);
        // Signed in just before this tick: the 20 s before were not used.
        t.tick(&[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 0);
        t.tick(&[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 20);
    }

    #[test]
    fn long_time_step_is_capped() {
        let mut t = Test::new();
        t.tick(&[session(1, JANIS)]);
        t.tick_by(Duration::from_secs(3600), &[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 40);
    }

    #[test]
    fn two_children_are_counted_apart() {
        let mut t = Test::new();
        for _ in 0..3 {
            t.tick(&[session(1, JANIS)]);
        }
        t.tick(&[session(2, PETERIS)]);
        t.tick(&[session(2, PETERIS)]);
        assert_eq!(t.used_s("janis"), 40);
        assert_eq!(t.used_s("peteris"), 20);
    }

    #[test]
    fn warnings_at_10_5_and_1_minute_once_each() {
        let mut t = Test::new();
        let s = [session(1, PETERIS)];
        let mut warnings = Vec::new();
        for _ in 0..(30 * 3 - 1) {
            for a in t.tick(&s) {
                match a {
                    Action::Warn {
                        minutes_left,
                        reason,
                        ..
                    } => {
                        assert_eq!(reason, Reason::Budget);
                        warnings.push(minutes_left);
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        assert_eq!(warnings, vec![10, 5, 1]);
    }

    #[test]
    fn time_up_then_logoff_after_grace() {
        let mut t = Test::new();
        let s = [session(1, PETERIS)];
        t.tick(&s);
        t.state.child_mut("peteris").used_ms = 30 * 60_000 - 10_000;

        let actions = t.tick(&s);
        assert_eq!(
            actions,
            vec![Action::TimeUp {
                session: 1,
                child_id: "peteris".into(),
                reason: Reason::Budget,
                grace_s: 10,
            }]
        );

        // Ticks come every second while a sign-out is pending.
        let mut ticks = 0;
        let actions = loop {
            ticks += 1;
            let actions = t.tick_by(Duration::from_secs(1), &s);
            if !actions.is_empty() {
                break actions;
            }
        };
        assert_eq!(ticks, 10);
        assert_eq!(
            actions,
            vec![Action::Logoff {
                session: 1,
                child_id: "peteris".into(),
                reason: Reason::Budget
            }]
        );

        // Sent again only if the session is still there after a while.
        assert!(t.tick_by(Duration::from_secs(9), &s).is_empty());
        assert_eq!(t.tick_by(Duration::from_secs(1), &s).len(), 1);
    }

    #[test]
    fn sign_in_after_time_ended_gets_short_grace() {
        let mut t = Test::new();
        t.tick(&[]);
        t.state.child_mut("peteris").used_ms = 30 * 60_000;

        let s = [session(7, PETERIS)];
        let actions = t.tick_by(Duration::from_millis(500), &s);
        assert!(matches!(
            actions[..],
            [Action::TimeUp {
                session: 7,
                grace_s: 5,
                ..
            }]
        ));
        assert!(t.tick_by(Duration::from_secs(4), &s).is_empty());
        assert!(matches!(
            t.tick_by(Duration::from_secs(1), &s)[..],
            [Action::Logoff { session: 7, .. }]
        ));
    }

    #[test]
    fn extra_time_cancels_sign_out_and_warns_again() {
        let mut t = Test::new();
        let s = [session(1, PETERIS)];
        while !matches!(t.tick(&s)[..], [Action::TimeUp { .. }]) {}
        t.state.child_mut("peteris").extra_ms += 15 * 60_000;
        assert!(t.tick(&s).is_empty()); // 14:40 left

        let mut warnings = Vec::new();
        for _ in 0..43 {
            for a in t.tick(&s) {
                if let Action::Warn { minutes_left, .. } = a {
                    warnings.push(minutes_left);
                } else {
                    panic!("unexpected {a:?}");
                }
            }
        }
        assert_eq!(warnings, vec![10, 5, 1]);
    }

    #[test]
    fn several_thresholds_at_once_give_one_warning() {
        let mut t = Test::new();
        t.tick(&[]);
        t.state.child_mut("peteris").used_ms = 26 * 60_000;
        let actions = t.tick(&[session(1, PETERIS)]);
        assert!(matches!(
            actions[..],
            [Action::Warn {
                minutes_left: 4,
                ..
            }]
        ));
        assert!(t.tick(&[session(1, PETERIS)]).is_empty());
    }

    #[test]
    fn outside_allowed_hours() {
        for now in [at(21, 30, 0), at(7, 0, 0)] {
            let mut t = Test { now, ..Test::new() };
            let actions = t.tick(&[session(1, JANIS)]);
            assert!(
                matches!(
                    actions[..],
                    [Action::TimeUp {
                        reason: Reason::Hours,
                        ..
                    }]
                ),
                "{now}"
            );
        }
    }

    #[test]
    fn warning_before_allowed_hours_end() {
        let mut t = Test::new();
        t.now = at(20, 49, 50);
        let actions = t.tick(&[session(1, JANIS)]);
        assert!(matches!(
            actions[..],
            [Action::Warn {
                minutes_left: 10,
                reason: Reason::Hours,
                ..
            }]
        ));
    }

    #[test]
    fn time_left_from_budget_or_hours() {
        let cfg = good();
        let janis = &cfg.children[0];
        let cs = ChildState {
            used_ms: 20 * 60_000,
            extra_ms: 15 * 60_000,
            ..Default::default()
        };
        let local = |utc| day::local_now(utc, cfg.utc_offset_minutes);

        // 60 + 15 - 20 = 55 minutes of budget, hours end much later.
        assert_eq!(
            time_left(janis, &cs, &local(at(12, 0, 0))),
            TimeLeft {
                left_ms: 55 * 60_000,
                limit_ms: 75 * 60_000,
                reason: Reason::Budget,
            }
        );
        // Allowed hours end at 21:00, in 10 minutes and 30 seconds.
        assert_eq!(
            time_left(janis, &cs, &local(at(20, 49, 30))),
            TimeLeft {
                left_ms: 10 * 60_000 + 30_000,
                limit_ms: 75 * 60_000,
                reason: Reason::Hours,
            }
        );
    }

    #[test]
    fn new_day_resets_used_time() {
        let mut t = Test::new();
        t.tick(&[session(1, JANIS)]);
        t.tick(&[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 20);
        t.now += chrono::TimeDelta::days(1);
        t.tick(&[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 20);
    }

    #[test]
    fn clock_back_to_yesterday_does_not_reset() {
        let mut t = Test::new();
        t.tick(&[session(1, JANIS)]);
        t.tick(&[session(1, JANIS)]);
        t.now -= chrono::TimeDelta::hours(13); // 23:00 the day before
        t.tick(&[session(1, JANIS)]);
        assert_eq!(t.used_s("janis"), 40);
    }

    #[test]
    fn saturday_limit() {
        let mut t = Test::new();
        t.now += chrono::TimeDelta::days(4); // Saturday
        t.tick(&[]);
        t.state.child_mut("janis").used_ms = 100 * 60_000;
        // 120 - 100 = 20 minutes left: no warning yet.
        assert!(t.tick(&[session(1, JANIS)]).is_empty());
    }

    #[test]
    fn next_point_is_the_next_warning_or_zero() {
        let min = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(until_next_point(25 * 60_000), Some(min(15)));
        // At 10:00 the 10-minute warning is done, so 5 minutes is next.
        assert_eq!(until_next_point(10 * 60_000), Some(min(5)));
        assert_eq!(until_next_point(60_001), Some(Duration::from_millis(1)));
        assert_eq!(until_next_point(30_000), Some(Duration::from_secs(30)));
        assert_eq!(until_next_point(0), None);
        assert_eq!(until_next_point(-5_000), None);
    }
}
