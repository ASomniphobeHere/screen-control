# Implementation Plan: Rust Service

This plan builds the service described in `specs/features.md` and `specs/technical-requirements.md`.

## Main idea

- Keep the rules (counting, limits, warnings, requests) in pure Rust code with no Windows calls. This code is easy to test with unit tests.
- Keep the Windows calls and the Telegram calls in thin layers around it.
- One thread owns all state. The other threads send it messages through a channel. This way no locks are needed.

## Project layout

```
Cargo.toml
src/
  main.rs            command line: service, console, check-config
  service.rs         Windows service wrapper
  app.rs             main loop: owns state, reads the channel
  core/
    config.rs        config types and checks
    state.rs         used time, extra time, requests
    day.rs           local date and time from UTC + offset
    engine.rs        one tick: sessions in, actions out
    commands.rs      Telegram commands: parse and apply
  win/
    sessions.rs      list sessions, user SID, locked or not
    clock.rs         QueryUnbiasedInterruptTime
    actions.rs       WTSSendMessageW, WTSLogoffSession
  telegram/
    api.rs           getUpdates, sendMessage, editMessageText, answerCallbackQuery
    bot.rs           polling thread
  config_sync.rs     download config from GitHub
  storage.rs         load and save JSON files safely
  logging.rs         log file setup
scripts/
  install.ps1
  uninstall.ps1
```

## Crates

- `windows-service` — service wrapper.
- `windows` — Win32 calls.
- `serde`, `serde_json` — JSON files.
- `chrono` — dates and week days.
- `reqwest` with the `blocking`, `json` and `rustls-tls` features — Telegram and GitHub.
- `tracing`, `tracing-subscriber`, `tracing-appender` — log file with rotation.
- `anyhow` — errors.
- `base64` — the GitHub API returns file content in base64.

No async runtime. Plain threads and `std::sync::mpsc` are enough.

## Threads

| Thread | Does | Sends to main loop |
|---|---|---|
| Main loop (`app.rs`) | Owns config and state. Runs a tick every 20 s. | — |
| Service control | Gets stop and session events from Windows. | `Stop`, `SessionChanged` |
| Telegram | Long polls `getUpdates`. | `TelegramUpdate` |
| Config sync | Checks GitHub every 10 min. | `NewConfig` |

The main loop reacts to each message and also runs a tick on a timer (`recv_timeout`). Telegram replies are sent from the main loop.

## Steps

Each step ends with something that works and can be tested.

### Step 1: Project setup

- `cargo new screen-control`, add crates.
- `main.rs` with three commands:
  - `service` — run as a Windows service (used by Windows).
  - `console` — run in a terminal, for testing.
  - `check-config <file>` — check a config file and print the errors.
- Logging to the terminal in `console` mode, to a file in `service` mode.

**Done when:** `cargo build --release` works and `console` starts and stops with Ctrl+C.

### Step 2: Config

- Types for the config: PC name, UTC offset, parents, children, limits per week day, allowed hours.
- Type for `secrets.json`: bot token, GitHub token, GitHub repo.
- Checks:
  - all required fields are there,
  - limits from 0 to 1440 minutes,
  - allowed hours in the form `HH:MM-HH:MM`,
  - child IDs, SIDs and Telegram IDs are unique,
  - at least one parent,
  - `pc_name` matches the PC name in `secrets.json`.
- `check-config` uses these checks.

**Done when:** unit tests pass for good configs and for each kind of bad config.

### Step 3: Day and time

- `local_now(utc, offset)` — local date and time from UTC plus the fixed offset. The system time zone is not used.
- If the UTC time is before the last seen UTC time (the clock went back), use the last seen time instead.
- `is_allowed(time, allowed_hours)` — also handles ranges over midnight, for example `22:00-02:00`.

**Done when:** unit tests pass for day change, clock going back and allowed hours.

### Step 4: State

- For each child: date, used seconds, extra seconds, warnings already sent today.
- Open requests: ID, child, minutes, time made, Telegram message ID for each parent.
- Last seen UTC time, last config SHA.
- `roll_day(date)` — sets used time, extra time and warnings to zero when the date changes.
- `storage.rs`: load, and save safely (write `*.tmp`, flush, then rename). If the state file is broken, log it and start with empty state for today.

**Done when:** unit tests pass, and a save followed by a load gives the same state.

### Step 5: Engine

The core of the program. A pure function:

```rust
fn tick(cfg: &Config, state: &mut State, now: LocalNow,
        delta: Duration, sessions: &[Session]) -> Vec<Action>
```

- `Session` = session ID, user SID, locked or not.
- `Action` = `Warn { session, minutes_left }`, `Logoff { session }`, `NotifyParents { text }`.
- Rules:
  - skip locked sessions and users not in the config,
  - cap `delta` at 2× the tick interval,
  - add `delta` to the child's used time,
  - time left = limit + extra − used,
  - warn once at 10, 5 and 1 minute,
  - log off when time left ≤ 0 or outside allowed hours,
  - a child signed in after time ended: log off after a 10 s grace.

**Done when:** unit tests cover all rules, with two children at the same time.

### Step 6: Windows layer

- `clock.rs` — `QueryUnbiasedInterruptTime` for `delta`.
- `sessions.rs`:
  - `WTSEnumerateSessionsW`, keep `WTSActive` sessions,
  - user SID: `WTSQueryUserToken` then `GetTokenInformation(TokenUser)`, then `ConvertSidToStringSidW`,
  - locked: `WTSQuerySessionInformationW` with `WTSSessionInfoEx`, read `SessionFlags`.
- `actions.rs` — `WTSSendMessageW` (no wait) and `WTSLogoffSession`.
- `app.rs` — main loop: get sessions → `tick` → run actions → save state.

`WTSQueryUserToken` works only as SYSTEM. To test in `console` mode, run it as SYSTEM, for example with `psexec -s -i`.

**Done when:** in `console` mode on a test PC, a test account with a 3-minute limit gets the warnings and is signed out.

### Step 7: Windows service

- `service.rs` with `windows-service`: register the handler, report `Running`, and accept `STOP` and `SESSIONCHANGE`.
- A session event sends `SessionChanged` to the main loop, which runs a tick at once. This catches a child who signs in after time ended.
- `Stop` saves state and ends the loop.

**Done when:** installed by hand with `sc create`, the service does the same as in Step 6, and a standard user cannot stop it.

### Step 8: Install script

`scripts/install.ps1` (run as Administrator):

1. Copy `screen-control.exe` to `C:\Program Files\ScreenControl\`.
2. Create `C:\ProgramData\ScreenTime\`. Set the ACL: only SYSTEM and Administrators, no inherited rights.
3. Ask for the PC name, bot token, GitHub token and repo, and write `secrets.json`.
4. Download the first config and check it with `check-config`.
5. `New-Service` with automatic start.
6. `sc failure` — restart after 5 s, every time.
7. Add the SafeBoot `Minimal` and `Network` registry keys.
8. Start the service.

`scripts/uninstall.ps1` does the reverse steps.

**Done when:** a fresh install on a test PC works, and running the script a second time updates the program without errors.

### Step 9: Telegram

- `api.rs` — small wrappers for `getUpdates` (timeout 50 s), `sendMessage` with an inline keyboard, `editMessageText`, `answerCallbackQuery`.
- `bot.rs` — polling thread. Saves the update offset. On a network error it waits and tries again (5 s, then up to 60 s).
- `commands.rs` — who sent it decides what is allowed:
  - child: `/more <min>`, `/status`,
  - parent: `/give`, `/take`, `/stop`, `/status`, `/reload`,
  - unknown: ignore and log.
- Requests:
  - make a short ID, send a message to each parent and save the message IDs,
  - callback data `approve:<id>:<min>` or `deny:<id>`, and check that the sender is a parent,
  - the first answer wins. Edit all parents' messages and send the answer to the child,
  - later presses get "Already answered by …",
  - remove requests after 30 minutes and edit the messages to "Expired".
- `/give`, `/take` and `/stop` send a short note to the other parents.
- `NotifyParents` actions from the engine are sent here.

The parsing and the request rules are pure code in `core/commands.rs`, with unit tests. Only sending is in `telegram/`.

**Done when:** a child can ask for time, two parents get the request, one approves, and the child gets the time and a reply.

### Step 10: Config updates

- `config_sync.rs` — `GET /repos/{owner}/{repo}/contents/pcs/<pc_name>.json` with the token from `secrets.json`.
- Compare the file SHA with the saved SHA. Do nothing if it is the same.
- Check the new config (Step 2). If it is good, save it safely and send `NewConfig` to the main loop. If not, keep the old one and tell the parents.
- Runs at start, every 10 minutes, and on `/reload`.
- Tell the parents: "PC-1: config updated (a1b2c3d)". Log the old SHA, the new SHA and what changed.

**Done when:** a push to the config repo reaches the PC, a broken config is rejected, and today's used time does not change.

### Step 11: Test on the real PCs

Use a spare standard account first, then the children's accounts. Check each item:

- [ ] Time counts only when unlocked.
- [ ] Sleep does not count.
- [ ] Warnings show at 10, 5 and 1 minute.
- [ ] Sign-out at zero, and again at sign-in after time ended.
- [ ] Allowed hours work, also over midnight if used.
- [ ] Changing the time zone does not change the day.
- [ ] The service cannot be stopped from a standard account or Task Manager.
- [ ] The service runs in Safe Mode.
- [ ] The service restarts after it is killed (as admin).
- [ ] State survives a power cut (hard power off).
- [ ] Telegram: request, approve, deny, expire, two parents, `/give`, `/take`, `/stop`, `/status`.
- [ ] Config update from GitHub, and a broken config.
- [ ] Without internet, limits still work.
- [ ] The log explains each sign-out.

Then remove Microsoft Family Safety from the children's accounts and run the service on both PCs.

### Step 12: Better warnings

A timer and better-looking warning windows, shown by a helper program in the child's session. This step has its own plan: `plans/better-warnings-plan.md`.

It can be done before or after step 11. The plain message boxes work until then.

**Done when:** the checklist in step 7 of that plan passes.

## Order and size

| Step | Size |
|---|---|
| 1–5 Core logic and tests | Medium. Most of the rules. |
| 6–8 Windows, service, install | Medium. Needs a real Windows PC to test. |
| 9 Telegram | Medium. |
| 10 Config updates | Small. |
| 11 Testing | Small, but do not skip it. |
| 12 Better warnings | Medium. See its own plan. |

After Step 8, the service already limits time on its own. Telegram and config updates can follow after that.
