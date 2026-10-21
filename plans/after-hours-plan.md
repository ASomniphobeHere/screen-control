# Implementation Plan: Extra Time After Hours

This plan builds "Extra time after hours" in `specs/features.md` and `specs/technical-requirements.md`.

## Main idea

- Today, `/give` outside the allowed hours adds to the budget, but the hours still block use. The grant does nothing. Just before the hours end, it gives only the minutes until the end.
- New: a grant of N minutes, when the allowed hours end in less than N minutes (or already ended), makes a **pass**: a start and an end in UTC. While the pass is active, time left is `end − now`. The budget and the hours are not used.
- The engine already signs out when time left reaches zero, warns at 10, 5 and 1 minute, and cancels a pending sign-out when time is added. A pass only changes how time left is found, so all of this keeps working.

## Changed files

```
src/
  core/
    state.rs      Pass type, passes in State, active pass lookup
    day.rs        LocalNow keeps the UTC time it was made from
    engine.rs     time_left uses the pass; Reason::Extra
    commands.rs   one grant function for /give and approve; /take, /stop and /status know passes
  app.rs          Notice texts for Reason::Extra; can_ask for Extra
  helper/popup.rs warning and time-up texts for Reason::Extra
README.md         the new rule, under Telegram commands
```

No new crates or `windows` features.

## Steps

### Step 1: Passes in the core

All in `src/core`, no Windows calls, so it is all unit tested.

- `state.rs`:
  - `Pass { start_utc, end_utc }`.
  - `State.passes: BTreeMap<String, Pass>` by child ID, `#[serde(default)]` so old state files load. `roll_day` does not clear it.
  - `State::active_pass(child_id, utc) -> Option<&Pass>`: active while `start ≤ utc < end`.
  - `State::drop_ended_passes(utc)`: removes passes that are not active. Called on each tick.
- `day.rs`: `LocalNow` gets a `utc` field, so `time_left` can compare with the pass without a new argument.
- `engine.rs`:
  - `Reason::Extra` (serde name `extra`).
  - `time_left(child, cs, pass, now)`: with an active pass, `left_ms = end − now`, `limit_ms = end − start`, reason `Extra`. Otherwise as now.
  - `tick` looks up the pass for each child and passes it in.
- `commands.rs`:
  - New `grant(cfg, state, now, child, minutes) -> Granted`, used by `/give` and the approve button.
    - Active pass: move its end later by `minutes`.
    - The allowed hours end in less than `minutes` from now (0 when outside the hours): new pass from now to now + `minutes`.
    - Otherwise: add to `extra_ms`, as now.
    - `Granted` says which, and the pass end, for the replies.
  - `/take` with an active pass: move its end earlier (it ends if the end is now or before). Otherwise as now.
  - `/stop`: remove the pass, then end the budget as now.
  - Replies with a pass: "Done. Jānis can use the PC until 22:10 (outside the allowed hours)." The same for the other parents. `/status` shows "until 22:10" instead of the time left.
  - The PC text stays "Mom gave you 30 minutes."
  - The local time in replies uses the config's UTC offset, like the day change.

**Tests:**

- `/give 30` at 21:40 (hours 08:00–21:00): pass until 22:10. `/give 15` again: until 22:25. `/take 10`: until 22:15.
- `/give 30` at 20:55: pass until 21:25.
- `/give 30` at 20:00 or at 20:30 (hours end in 30 min, not less): adds to `extra_ms`, no pass (today's tests still pass).
- Approve button at 21:40: pass, same as `/give`.
- `/stop` during a pass: pass removed, time left 0.
- Engine: with a pass, time left counts down on the clock, also while the session is locked or gone; warnings at 10, 5 and 1 with reason `Extra`; time-up at the end, then the reason is `Hours` again.
- Engine: a `/give` during the time-up grace cancels the sign-out (as with the budget today).
- Pass over midnight: still active after the new day, and `roll_day` keeps it.
- Clock put back before the pass start: pass not active. Clock forward past the end: pass ended.
- A child with no allowed hours: `/give` always adds to the budget.
- Old `state.json` without `passes` loads.

**Done when:** `cargo test` and `cargo clippy` pass.

### Step 2: Texts, service and VM test

- `app.rs`:
  - `Notice::warn` with `Reason::Extra`: "{name}, your extra time ends in {n} minutes."
  - `Notice::time_up` with `Reason::Extra`: "your extra time is over". `can_ask` is true for `Extra` too, so the child can ask again.
  - `next_point` and `send_status` pass the active pass to `time_left`. Nothing else changes: the early tick and the helper status work from `left_ms`.
- `helper/popup.rs`: `warn_text` and `time_up_text` for `Reason::Extra`, same words as above. Add `helper --show warn-extra` to the debug samples.
- `README.md`: one paragraph under Telegram commands.
- Tick the checklist below.

**Test in the VM** (it needs a test config with allowed hours that end soon, and Telegram messages from a parent account):

The VM config has no allowed hours for `testkid`, and its budget was used up. So the pass was added to `state.json` by hand (service stopped, pass from now to now + 7 min, service started), not by a parent's `/give`.

- [x] Outside the hours, `testkid` is signed out. `/give 5` from a parent: `testkid` can sign in, the timer shows 5:00 and counts down, also while the screen is locked. (VM, pass added by hand: with the budget used up, `testkid` could sign in, the timer showed 7 min and counted down. Locked for about 1 minute: the time left went down by the same, and the 5-minute warning came after unlocking. The real `/give` is left for the user.)
- [ ] Inside the hours, a few minutes before they end, `/give 5`: the timer shows 5:00, past the hours end. (Left for the user: needs a config with allowed hours and a parent's `/give`. Covered by unit tests.)
- [x] Warning at 1 minute says "extra time". Time-up at 0:00, then the sign-out. (VM: warnings at 5 and 1 minute "your extra time ends soon", time-up at 0:00 "your extra time is over", signed out 10 s later.)
- [x] Signing in again after the end: signed out after a few seconds, with reason "not allowed at this hour". (VM: signed out after 5 s. The reason was "used up", because the VM config has no allowed hours. The hours reason after a pass is covered by unit tests.)
- [ ] `/status` shows "until hh:mm" during the pass. (Left for the user: needs Telegram. Covered by unit tests.)
- [x] Service restart during the pass: the pass is kept, with the same end. (VM)

## Order and size

| Step | Size |
|---|---|
| 1 Passes in the core | Medium. Most of the work is the command replies and tests. |
| 2 Texts, service and VM test | Small. |
