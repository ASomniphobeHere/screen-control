//! Which children's sessions need the helper started. Pure, so it can be tested.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Time between tries for the same session, so a broken helper is not started again and again.
/// A started helper also needs a moment to connect.
pub const RETRY: Duration = Duration::from_secs(10);

/// The last try to start the helper in each session.
#[derive(Default)]
pub struct HelperStarts {
    last_try: HashMap<u32, Instant>,
}

impl HelperStarts {
    /// Returns the sessions to start the helper in now, and counts them as tried at `now`.
    /// `child_sessions` are the active sessions of children; `connected` have a helper.
    pub fn due(
        &mut self,
        now: Instant,
        child_sessions: &[u32],
        connected: &HashSet<u32>,
    ) -> Vec<u32> {
        // Ended sessions are forgotten. A new session with the same ID starts fresh.
        self.last_try.retain(|s, _| child_sessions.contains(s));
        let mut due = Vec::new();
        for &session in child_sessions {
            if connected.contains(&session) {
                continue;
            }
            let waited = self
                .last_try
                .get(&session)
                .is_none_or(|t| now.saturating_duration_since(*t) >= RETRY);
            if waited {
                self.last_try.insert(session, now);
                due.push(session);
            }
        }
        due
    }
}

/// Children's sessions that have no connected helper.
pub fn without_helper(child_sessions: &[u32], connected: &HashSet<u32>) -> Vec<u32> {
    child_sessions
        .iter()
        .copied()
        .filter(|s| !connected.contains(s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn starts_in_child_sessions_without_a_helper() {
        let mut starts = HelperStarts::default();
        let now = Instant::now();
        assert_eq!(starts.due(now, &[1, 2, 3], &set(&[2])), vec![1, 3]);
    }

    #[test]
    fn waits_between_tries() {
        let mut starts = HelperStarts::default();
        let t0 = Instant::now();
        assert_eq!(starts.due(t0, &[1], &set(&[])), vec![1]);
        // The helper has not connected yet: no second start.
        assert!(
            starts
                .due(t0 + Duration::from_secs(1), &[1], &set(&[]))
                .is_empty()
        );
        assert!(
            starts
                .due(t0 + Duration::from_secs(9), &[1], &set(&[]))
                .is_empty()
        );
        assert_eq!(starts.due(t0 + RETRY, &[1], &set(&[])), vec![1]);
    }

    #[test]
    fn starts_again_soon_after_the_helper_goes() {
        let mut starts = HelperStarts::default();
        let t0 = Instant::now();
        assert_eq!(starts.due(t0, &[1], &set(&[])), vec![1]);
        // Connected for a long time, then killed.
        let later = t0 + Duration::from_secs(600);
        assert!(starts.due(later, &[1], &set(&[1])).is_empty());
        assert_eq!(
            starts.due(later + Duration::from_secs(1), &[1], &set(&[])),
            vec![1]
        );
    }

    #[test]
    fn new_session_with_the_same_id_starts_at_once() {
        let mut starts = HelperStarts::default();
        let t0 = Instant::now();
        assert_eq!(starts.due(t0, &[1], &set(&[])), vec![1]);
        // Session 1 ends, and a new session 1 begins 2 seconds later.
        assert!(
            starts
                .due(t0 + Duration::from_secs(1), &[], &set(&[]))
                .is_empty()
        );
        assert_eq!(
            starts.due(t0 + Duration::from_secs(2), &[1], &set(&[])),
            vec![1]
        );
    }

    #[test]
    fn sessions_without_helper() {
        assert_eq!(without_helper(&[1, 2, 3], &set(&[2, 5])), vec![1, 3]);
        assert!(without_helper(&[2], &set(&[2])).is_empty());
        assert!(without_helper(&[], &set(&[])).is_empty());
    }
}
