//! Telegram commands and extra time requests. Messages in, messages out. No network calls here.

use chrono::{DateTime, TimeDelta, Utc};

use super::config::{Child, Config, Parent};
use super::day;
use super::engine::budget_left_ms;
use super::state::{ParentMessage, Request, State};

/// Requests with no answer are removed after this time.
pub const REQUEST_TIMEOUT: TimeDelta = TimeDelta::minutes(30);

/// The most minutes that can be asked for, given or taken at once.
pub const MAX_MINUTES: u32 = 600;

/// Text messages older than this are ignored, for example after the PC was off.
pub const MAX_MESSAGE_AGE: TimeDelta = TimeDelta::minutes(10);

pub enum Input<'a> {
    Text {
        from: i64,
        text: &'a str,
    },
    Button {
        from: i64,
        callback_id: &'a str,
        data: &'a str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub text: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// `request_id` is set for a request sent to a parent, so its message ID can be saved.
    Send {
        to: i64,
        text: String,
        buttons: Vec<Button>,
        request_id: Option<String>,
    },
    Edit {
        to: i64,
        message_id: i64,
        text: String,
    },
    /// Every button press must be answered, or Telegram shows a loading sign.
    Answer { callback_id: String, text: String },
    /// Show a message on the PC to a child who is signed in.
    NotifyPc { child_id: String, text: String },
}

fn send(to: i64, text: impl Into<String>) -> Out {
    Out::Send {
        to,
        text: text.into(),
        buttons: Vec::new(),
        request_id: None,
    }
}

pub fn handle(cfg: &Config, state: &mut State, now: DateTime<Utc>, input: Input) -> Vec<Out> {
    roll_day(cfg, state, now);
    match input {
        Input::Text { from, text } => {
            if let Some(parent) = cfg.parents.iter().find(|p| p.telegram_user_id == from) {
                parent_command(cfg, state, now, parent, text)
            } else if let Some(child) = cfg
                .children
                .iter()
                .find(|c| c.telegram_user_id == Some(from))
            {
                child_command(cfg, state, now, child, text)
            } else {
                tracing::warn!(from, text, "message from an unknown Telegram user, ignored");
                Vec::new()
            }
        }
        Input::Button {
            from,
            callback_id,
            data,
        } => {
            let answer = |text: &str| Out::Answer {
                callback_id: callback_id.to_string(),
                text: text.to_string(),
            };
            match cfg.parents.iter().find(|p| p.telegram_user_id == from) {
                Some(parent) => button(cfg, state, parent, data, answer),
                None => {
                    tracing::warn!(
                        from,
                        data,
                        "button press from someone who is not a parent, ignored"
                    );
                    vec![answer("Only parents can answer requests.")]
                }
            }
        }
    }
}

/// Removes requests that got no answer in time.
pub fn expire(cfg: &Config, state: &mut State, now: DateTime<Utc>) -> Vec<Out> {
    let (old, keep): (Vec<Request>, Vec<Request>) = state
        .requests
        .drain(..)
        .partition(|r| now - r.created_utc >= REQUEST_TIMEOUT);
    state.requests = keep;
    let mut out = Vec::new();
    for r in old {
        tracing::info!(request = %r.id, child = %r.child_id, "request expired");
        let name = child_name(cfg, &r.child_id);
        out.extend(edit_request(
            &r,
            format!(
                "⌛ {name} asked for {} min. No answer, the request expired.",
                r.minutes
            ),
        ));
        out.extend(to_child(
            cfg,
            &r.child_id,
            "Your request for more time expired. You can ask again.",
        ));
    }
    out
}

/// Saves the message ID of a request sent to a parent, so it can be edited later.
pub fn request_sent(state: &mut State, request_id: &str, to: i64, message_id: i64) {
    if let Some(r) = state.requests.iter_mut().find(|r| r.id == request_id) {
        r.parent_messages.push(ParentMessage {
            telegram_user_id: to,
            message_id,
        });
    }
}

fn roll_day(cfg: &Config, state: &mut State, now: DateTime<Utc>) {
    let utc = day::effective_utc(now, state.last_seen_utc);
    state.roll_day(day::local_now(utc, cfg.utc_offset_minutes).date);
}

/// The first word, lower case, without a `@botname` ending, and the other words.
fn split_command(text: &str) -> (String, Vec<&str>) {
    let mut words = text.split_whitespace();
    let command = words.next().unwrap_or("").to_lowercase();
    let command = command.split('@').next().unwrap_or("").to_string();
    (command, words.collect())
}

fn parse_minutes(word: Option<&&str>) -> Result<u32, String> {
    match word.and_then(|w| w.parse::<u32>().ok()) {
        Some(m) if (1..=MAX_MINUTES).contains(&m) => Ok(m),
        _ => Err(format!(
            "Give the minutes as a number from 1 to {MAX_MINUTES}."
        )),
    }
}

fn child_command(
    cfg: &Config,
    state: &mut State,
    now: DateTime<Utc>,
    child: &Child,
    text: &str,
) -> Vec<Out> {
    let me = child.telegram_user_id.unwrap_or_default();
    let (command, args) = split_command(text);
    match command.as_str() {
        "/more" => {
            let minutes = match parse_minutes(args.first()) {
                Ok(m) => m,
                Err(e) => return vec![send(me, format!("{e} Example: /more 30"))],
            };
            if state.requests.iter().any(|r| r.child_id == child.id) {
                return vec![send(me, "You already asked. Wait for an answer.")];
            }
            state.next_request_id += 1;
            let request = Request {
                id: format!("r{}", state.next_request_id),
                child_id: child.id.clone(),
                minutes,
                created_utc: now,
                parent_messages: Vec::new(),
            };
            let left = time_left(cfg, state, now, child);
            let text = format!(
                "🙋 {} asks for {minutes} min on {} ({left} left today).",
                child.name, cfg.pc_name
            );
            let mut buttons = vec![Button {
                text: format!("Approve {minutes} min"),
                data: format!("approve:{}:{minutes}", request.id),
            }];
            if minutes > 15 {
                buttons.push(Button {
                    text: "Approve 15 min".into(),
                    data: format!("approve:{}:15", request.id),
                });
            }
            buttons.push(Button {
                text: "Deny".into(),
                data: format!("deny:{}", request.id),
            });

            let mut out: Vec<Out> = cfg
                .parents
                .iter()
                .map(|p| Out::Send {
                    to: p.telegram_user_id,
                    text: text.clone(),
                    buttons: buttons.clone(),
                    request_id: Some(request.id.clone()),
                })
                .collect();
            tracing::info!(request = %request.id, child = %child.id, minutes, "extra time requested");
            state.requests.push(request);
            out.push(send(
                me,
                format!("Asked your parents for {minutes} more minutes."),
            ));
            out
        }
        "/status" => vec![send(
            me,
            format!("You have {} left today.", time_left(cfg, state, now, child)),
        )],
        _ => vec![send(
            me,
            "Commands:\n/more <minutes> — ask for more time, for example /more 30\n/status — your time left today",
        )],
    }
}

fn parent_command(
    cfg: &Config,
    state: &mut State,
    now: DateTime<Utc>,
    parent: &Parent,
    text: &str,
) -> Vec<Out> {
    let me = parent.telegram_user_id;
    let (command, args) = split_command(text);
    // With only one child on this PC, the name can be left out: `/give 30`, `/stop`.
    let only_child = if cfg.children.len() == 1 {
        cfg.children.first()
    } else {
        None
    };
    let find_child = |name: Option<&&str>| -> Result<&Child, String> {
        let Some(name) = name else {
            return only_child.ok_or_else(|| "Give the child's name.".to_string());
        };
        let name = name.to_lowercase();
        cfg.children
            .iter()
            .find(|c| c.id.to_lowercase() == name || c.name.to_lowercase() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = cfg.children.iter().map(|c| c.name.as_str()).collect();
                format!(
                    "No child called \"{name}\". Children: {}.",
                    names.join(", ")
                )
            })
    };

    match command.as_str() {
        "/give" | "/take" => {
            let (name, minutes) = if only_child.is_some() && args.len() <= 1 {
                (None, args.first())
            } else {
                (args.first(), args.get(1))
            };
            let (child, minutes) = match (find_child(name), parse_minutes(minutes)) {
                (Ok(c), Ok(m)) => (c, m),
                (Err(e), _) | (_, Err(e)) => {
                    return vec![send(
                        me,
                        format!("{e} Example: {command} {}30", name_example(cfg)),
                    )];
                }
            };
            let give = command == "/give";
            let ms = minutes as i64 * 60_000;
            state.child_mut(&child.id).extra_ms += if give { ms } else { -ms };
            tracing::info!(parent = %parent.name, child = %child.id, minutes, give, "extra time changed");
            let left = time_left(cfg, state, now, child);
            let (verb, pc_text) = if give {
                (
                    "gave",
                    format!("{} gave you {minutes} more minutes.", parent.name),
                )
            } else {
                (
                    "took",
                    format!("{} took {minutes} minutes of your time.", parent.name),
                )
            };
            let mut out = vec![send(
                me,
                format!("Done. {} has {left} left today.", child.name),
            )];
            out.extend(to_other_parents(
                cfg,
                me,
                format!(
                    "{} {verb} {} {minutes} min. {left} left today.",
                    parent.name, child.name
                ),
            ));
            out.extend(to_child(cfg, &child.id, &pc_text));
            out.push(Out::NotifyPc {
                child_id: child.id.clone(),
                text: pc_text,
            });
            out
        }
        "/stop" => {
            let child = match find_child(args.first()) {
                Ok(c) => c,
                Err(e) => {
                    return vec![send(
                        me,
                        format!(
                            "{e} Example: {}",
                            format!("/stop {}", name_example(cfg)).trim_end()
                        ),
                    )];
                }
            };
            let weekday = local_weekday(cfg, state, now);
            let cs = state.child_mut(&child.id);
            // Extra time below zero, so that exactly nothing is left.
            cs.extra_ms -= budget_left_ms(child, cs, weekday).max(0);
            tracing::info!(parent = %parent.name, child = %child.id, "time stopped");
            let pc_text = format!("{} ended your screen time for today.", parent.name);
            let mut out = vec![send(
                me,
                format!("Done. {} has no time left today.", child.name),
            )];
            out.extend(to_other_parents(
                cfg,
                me,
                format!("{} ended {}'s time for today.", parent.name, child.name),
            ));
            out.extend(to_child(cfg, &child.id, &pc_text));
            out
        }
        "/status" => {
            let lines: Vec<String> = cfg
                .children
                .iter()
                .map(|c| {
                    let cs = state.children.get(&c.id).cloned().unwrap_or_default();
                    let mut line = format!(
                        "{}: {} left (used {}",
                        c.name,
                        time_left(cfg, state, now, c),
                        format_ms(cs.used_ms as i64)
                    );
                    if cs.extra_ms != 0 {
                        line += &format!(
                            ", extra {}{}",
                            if cs.extra_ms < 0 { "-" } else { "+" },
                            format_ms(cs.extra_ms.abs())
                        );
                    }
                    line + ")"
                })
                .collect();
            let open = state.requests.len();
            let mut text = format!("{}:\n{}", cfg.pc_name, lines.join("\n"));
            if open > 0 {
                text += &format!("\nOpen requests: {open}");
            }
            vec![send(me, text)]
        }
        _ if only_child.is_some() => vec![send(
            me,
            "Commands:\n/status — time left today\n/give <minutes> — give extra time\n/take <minutes> — take time away\n/stop — end today's time now\n\nExample: /give 30",
        )],
        _ => vec![send(
            me,
            format!(
                "Commands:\n/status — time for all children\n/give <name> <minutes> — give extra time\n/take <name> <minutes> — take time away\n/stop <name> — end today's time now\n\nExample: /give {}30",
                name_example(cfg)
            ),
        )],
    }
}

fn button(
    cfg: &Config,
    state: &mut State,
    parent: &Parent,
    data: &str,
    answer: impl Fn(&str) -> Out,
) -> Vec<Out> {
    let mut parts = data.split(':');
    let (action, id) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let Some(index) = state.requests.iter().position(|r| r.id == id) else {
        return vec![answer("This request was already answered or has expired.")];
    };
    let request = state.requests.remove(index);
    let name = child_name(cfg, &request.child_id);

    match (action, parts.next().and_then(|m| m.parse::<u32>().ok())) {
        ("approve", Some(minutes)) if (1..=MAX_MINUTES).contains(&minutes) => {
            state.child_mut(&request.child_id).extra_ms += minutes as i64 * 60_000;
            tracing::info!(request = %id, parent = %parent.name, child = %request.child_id, minutes, "request approved");
            let pc_text = format!("{} gave you {minutes} more minutes.", parent.name);
            let mut out = vec![answer("Done.")];
            out.extend(edit_request(
                &request,
                format!(
                    "✅ {name} asked for {} min. {} gave {minutes} min.",
                    request.minutes, parent.name
                ),
            ));
            out.extend(to_child(cfg, &request.child_id, &pc_text));
            out.push(Out::NotifyPc {
                child_id: request.child_id.clone(),
                text: pc_text,
            });
            out
        }
        ("deny", _) => {
            tracing::info!(request = %id, parent = %parent.name, child = %request.child_id, "request denied");
            let mut out = vec![answer("Done.")];
            out.extend(edit_request(
                &request,
                format!(
                    "❌ {name} asked for {} min. {} said no.",
                    request.minutes, parent.name
                ),
            ));
            out.extend(to_child(
                cfg,
                &request.child_id,
                &format!("{} said no to more time.", parent.name),
            ));
            out
        }
        _ => {
            tracing::warn!(data, "unknown button");
            state.requests.insert(index, request);
            vec![answer("Unknown button.")]
        }
    }
}

fn edit_request(request: &Request, text: String) -> Vec<Out> {
    request
        .parent_messages
        .iter()
        .map(|m| Out::Edit {
            to: m.telegram_user_id,
            message_id: m.message_id,
            text: text.clone(),
        })
        .collect()
}

fn to_child(cfg: &Config, child_id: &str, text: &str) -> Option<Out> {
    let child = cfg.children.iter().find(|c| c.id == child_id)?;
    Some(send(child.telegram_user_id?, text))
}

fn to_other_parents(cfg: &Config, me: i64, text: String) -> Vec<Out> {
    cfg.parents
        .iter()
        .filter(|p| p.telegram_user_id != me)
        .map(|p| send(p.telegram_user_id, text.clone()))
        .collect()
}

fn child_name(cfg: &Config, child_id: &str) -> String {
    cfg.children
        .iter()
        .find(|c| c.id == child_id)
        .map_or(child_id.to_string(), |c| c.name.clone())
}

/// The name part of an example command, like `janis `. Empty when there is only one child.
fn name_example(cfg: &Config) -> String {
    match cfg.children.as_slice() {
        [_] => String::new(),
        [first, ..] => format!("{} ", first.id),
        [] => "name ".to_string(),
    }
}

fn local_weekday(cfg: &Config, state: &State, now: DateTime<Utc>) -> chrono::Weekday {
    day::local_now(
        day::effective_utc(now, state.last_seen_utc),
        cfg.utc_offset_minutes,
    )
    .weekday
}

fn time_left(cfg: &Config, state: &State, now: DateTime<Utc>, child: &Child) -> String {
    let cs = state.children.get(&child.id).cloned().unwrap_or_default();
    format_ms(budget_left_ms(child, &cs, local_weekday(cfg, state, now)))
}

/// `1 h 5 min`, `45 min`, `0 min`. Rounded down.
pub fn format_ms(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::tests::good;
    use chrono::TimeZone;

    // From the test config: parents Mom 111 and Dad 333, child janis 222 (60 min on Tuesday),
    // child peteris with no Telegram (30 min).
    const MOM: i64 = 111;
    const DAD: i64 = 333;
    const JANIS: i64 = 222;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 9, 0, 0).unwrap() // Tuesday 12:00 at UTC+3
    }

    fn text(cfg: &Config, state: &mut State, from: i64, text: &str) -> Vec<Out> {
        handle(cfg, state, now(), Input::Text { from, text })
    }

    fn press(cfg: &Config, state: &mut State, from: i64, data: &str) -> Vec<Out> {
        handle(
            cfg,
            state,
            now(),
            Input::Button {
                from,
                callback_id: "cb",
                data,
            },
        )
    }

    fn texts_to(out: &[Out], to: i64) -> Vec<String> {
        out.iter()
            .filter_map(|o| match o {
                Out::Send { to: t, text, .. } | Out::Edit { to: t, text, .. } if *t == to => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Asks for time as Jānis and saves message IDs as if Telegram sent the requests.
    fn ask(cfg: &Config, state: &mut State, minutes: u32) -> String {
        let out = text(cfg, state, JANIS, &format!("/more {minutes}"));
        let mut id = String::new();
        for (n, o) in out.iter().enumerate() {
            if let Out::Send {
                to,
                request_id: Some(r),
                ..
            } = o
            {
                request_sent(state, r, *to, n as i64 + 100);
                id = r.clone();
            }
        }
        id
    }

    #[test]
    fn request_goes_to_all_parents_with_buttons() {
        let (cfg, mut state) = (good(), State::default());
        let out = text(&cfg, &mut state, JANIS, "/more@sc_test_bot 30");
        let to_parents: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                Out::Send {
                    to,
                    text,
                    buttons,
                    request_id: Some(_),
                } => Some((*to, text.clone(), buttons.len())),
                _ => None,
            })
            .collect();
        assert_eq!(to_parents.len(), 2);
        assert_eq!(
            to_parents[0],
            (
                MOM,
                "🙋 Jānis asks for 30 min on PC-1 (1 h left today).".into(),
                3
            )
        );
        assert_eq!(
            texts_to(&out, JANIS),
            ["Asked your parents for 30 more minutes."]
        );
        assert_eq!(state.requests.len(), 1);
    }

    #[test]
    fn short_request_has_no_15_minute_button() {
        let (cfg, mut state) = (good(), State::default());
        let out = text(&cfg, &mut state, JANIS, "/more 10");
        assert!(matches!(&out[0], Out::Send { buttons, .. } if buttons.len() == 2));
    }

    #[test]
    fn only_one_open_request_per_child() {
        let (cfg, mut state) = (good(), State::default());
        ask(&cfg, &mut state, 30);
        let out = text(&cfg, &mut state, JANIS, "/more 20");
        assert_eq!(
            texts_to(&out, JANIS),
            ["You already asked. Wait for an answer."]
        );
        assert_eq!(state.requests.len(), 1);
    }

    #[test]
    fn bad_minutes() {
        let (cfg, mut state) = (good(), State::default());
        for bad in ["/more", "/more abc", "/more 0", "/more 601"] {
            let out = text(&cfg, &mut state, JANIS, bad);
            assert!(
                texts_to(&out, JANIS)[0].starts_with("Give the minutes"),
                "{bad}"
            );
        }
        assert!(state.requests.is_empty());
    }

    #[test]
    fn first_approve_wins_and_edits_all_copies() {
        let (cfg, mut state) = (good(), State::default());
        let id = ask(&cfg, &mut state, 30);

        let out = press(&cfg, &mut state, DAD, &format!("approve:{id}:15"));
        assert_eq!(
            out[0],
            Out::Answer {
                callback_id: "cb".into(),
                text: "Done.".into()
            }
        );
        assert_eq!(
            texts_to(&out, MOM),
            ["✅ Jānis asked for 30 min. Dad gave 15 min."]
        );
        assert_eq!(
            texts_to(&out, DAD),
            ["✅ Jānis asked for 30 min. Dad gave 15 min."]
        );
        assert_eq!(texts_to(&out, JANIS), ["Dad gave you 15 more minutes."]);
        assert!(out.contains(&Out::NotifyPc {
            child_id: "janis".into(),
            text: "Dad gave you 15 more minutes.".into()
        }));
        assert_eq!(state.child_mut("janis").extra_ms, 15 * 60_000);

        // Mom presses later: nothing changes.
        let out = press(&cfg, &mut state, MOM, &format!("approve:{id}:30"));
        assert_eq!(
            out,
            [Out::Answer {
                callback_id: "cb".into(),
                text: "This request was already answered or has expired.".into()
            }]
        );
        assert_eq!(state.child_mut("janis").extra_ms, 15 * 60_000);
    }

    #[test]
    fn deny() {
        let (cfg, mut state) = (good(), State::default());
        let id = ask(&cfg, &mut state, 30);
        let out = press(&cfg, &mut state, MOM, &format!("deny:{id}"));
        assert_eq!(
            texts_to(&out, DAD),
            ["❌ Jānis asked for 30 min. Mom said no."]
        );
        assert_eq!(texts_to(&out, JANIS), ["Mom said no to more time."]);
        assert!(state.requests.is_empty());
        assert_eq!(state.child_mut("janis").extra_ms, 0);
    }

    #[test]
    fn child_cannot_press_buttons_or_use_parent_commands() {
        let (cfg, mut state) = (good(), State::default());
        let id = ask(&cfg, &mut state, 30);
        let out = press(&cfg, &mut state, JANIS, &format!("approve:{id}:30"));
        assert_eq!(
            out,
            [Out::Answer {
                callback_id: "cb".into(),
                text: "Only parents can answer requests.".into()
            }]
        );
        assert_eq!(state.requests.len(), 1);

        let out = text(&cfg, &mut state, JANIS, "/give janis 600");
        assert!(texts_to(&out, JANIS)[0].starts_with("Commands:"));
        assert_eq!(state.child_mut("janis").extra_ms, 0);
    }

    #[test]
    fn unknown_user_is_ignored() {
        let (cfg, mut state) = (good(), State::default());
        assert!(text(&cfg, &mut state, 999, "/more 30").is_empty());
        assert!(text(&cfg, &mut state, 999, "/give janis 30").is_empty());
        assert!(state.requests.is_empty());
    }

    #[test]
    fn give_and_take_by_name_or_id() {
        let (cfg, mut state) = (good(), State::default());
        let out = text(&cfg, &mut state, MOM, "/give JĀNIS 30");
        assert_eq!(
            texts_to(&out, MOM),
            ["Done. Jānis has 1 h 30 min left today."]
        );
        assert_eq!(
            texts_to(&out, DAD),
            ["Mom gave Jānis 30 min. 1 h 30 min left today."]
        );
        assert_eq!(texts_to(&out, JANIS), ["Mom gave you 30 more minutes."]);

        text(&cfg, &mut state, DAD, "/take janis 45");
        assert_eq!(state.child_mut("janis").extra_ms, -15 * 60_000);

        // Pēteris has no Telegram: no message to him, but still a message on the PC.
        let out = text(&cfg, &mut state, MOM, "/give peteris 5");
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::NotifyPc { child_id, .. } if child_id == "peteris"))
        );
    }

    #[test]
    fn give_with_mistakes() {
        let (cfg, mut state) = (good(), State::default());
        let out = text(&cfg, &mut state, MOM, "/give anna 30");
        assert_eq!(
            texts_to(&out, MOM),
            ["No child called \"anna\". Children: Jānis, Pēteris. Example: /give janis 30"]
        );
        let out = text(&cfg, &mut state, MOM, "/give janis");
        assert!(texts_to(&out, MOM)[0].starts_with("Give the minutes"));
    }

    #[test]
    fn with_one_child_the_name_can_be_left_out() {
        let mut cfg = good();
        cfg.children.truncate(1);
        let mut state = State::default();

        let out = text(&cfg, &mut state, MOM, "/give 30");
        assert_eq!(
            texts_to(&out, MOM),
            ["Done. Jānis has 1 h 30 min left today."]
        );
        text(&cfg, &mut state, MOM, "/take 10");
        assert_eq!(state.child_mut("janis").extra_ms, 20 * 60_000);
        // The name still works.
        text(&cfg, &mut state, MOM, "/give janis 5");
        assert_eq!(state.child_mut("janis").extra_ms, 25 * 60_000);

        let out = text(&cfg, &mut state, MOM, "/stop");
        assert_eq!(texts_to(&out, MOM), ["Done. Jānis has no time left today."]);

        let out = text(&cfg, &mut state, MOM, "/give");
        assert_eq!(
            texts_to(&out, MOM),
            ["Give the minutes as a number from 1 to 600. Example: /give 30"]
        );
        let out = text(&cfg, &mut state, MOM, "/help");
        assert!(texts_to(&out, MOM)[0].contains("/give <minutes>"));
    }

    #[test]
    fn with_two_children_the_name_is_needed() {
        let (cfg, mut state) = (good(), State::default());
        let out = text(&cfg, &mut state, MOM, "/give 30");
        assert!(texts_to(&out, MOM)[0].starts_with("No child called \"30\""));
        let out = text(&cfg, &mut state, MOM, "/stop");
        assert_eq!(
            texts_to(&out, MOM),
            ["Give the child's name. Example: /stop janis"]
        );
        assert_eq!(state.child_mut("janis").extra_ms, 0);
    }

    #[test]
    fn stop_leaves_nothing() {
        let (cfg, mut state) = (good(), State::default());
        state.roll_day(chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        state.child_mut("janis").used_ms = 20 * 60_000;
        text(&cfg, &mut state, MOM, "/stop janis");
        let cs = state.child_mut("janis").clone();
        assert_eq!(
            budget_left_ms(&cfg.children[0], &cs, chrono::Weekday::Tue),
            0
        );
    }

    #[test]
    fn status_for_parent_and_child() {
        let (cfg, mut state) = (good(), State::default());
        state.roll_day(chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        state.child_mut("janis").used_ms = 15 * 60_000;
        state.child_mut("janis").extra_ms = 10 * 60_000;
        let out = text(&cfg, &mut state, MOM, "/status");
        assert_eq!(
            texts_to(&out, MOM),
            [
                "PC-1:\nJānis: 55 min left (used 15 min, extra +10 min)\nPēteris: 30 min left (used 0 min)"
            ]
        );
        let out = text(&cfg, &mut state, JANIS, "/status");
        assert_eq!(texts_to(&out, JANIS), ["You have 55 min left today."]);
    }

    #[test]
    fn requests_expire() {
        let (cfg, mut state) = (good(), State::default());
        ask(&cfg, &mut state, 30);
        assert!(expire(&cfg, &mut state, now() + TimeDelta::minutes(29)).is_empty());
        let out = expire(&cfg, &mut state, now() + REQUEST_TIMEOUT);
        assert_eq!(
            texts_to(&out, MOM),
            ["⌛ Jānis asked for 30 min. No answer, the request expired."]
        );
        assert_eq!(
            texts_to(&out, JANIS),
            ["Your request for more time expired. You can ask again."]
        );
        assert!(state.requests.is_empty());
    }

    #[test]
    fn time_format() {
        assert_eq!(format_ms(-5), "0 min");
        assert_eq!(format_ms(59_999), "0 min");
        assert_eq!(format_ms(45 * 60_000), "45 min");
        assert_eq!(format_ms(120 * 60_000), "2 h");
        assert_eq!(format_ms(65 * 60_000), "1 h 5 min");
    }
}
