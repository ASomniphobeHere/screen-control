# Implementation Plan: Better Warnings

This plan builds the "Better warnings" feature in `specs/features.md` and the "Helper program" section in `specs/technical-requirements.md`. It is step 12 of `plans/implementation-plan.md`.

## Main idea

- The service stays in charge. It counts time and signs users out, as now.
- A helper program runs in each child's session and only shows things: a timer, warning windows, and a time-up window.
- The service sends the helper messages through a named pipe. Messages go one way: service to helper.
- If the helper is not running, the service uses the plain message boxes, as now. Killing the helper never removes the limits.

## Project layout

New and changed files:

```
src/
  helper_protocol.rs   messages from the service to the helper (pure, tested)
  service.rs           unchanged
  app.rs               sends messages to helpers, uses message boxes only without a helper
  win/
    pipe_server.rs     the named pipe in the service, one client per session
    launch.rs          starts the helper in a child's session
  helper/
    mod.rs             `screen-control helper`: connects to the pipe, runs the windows
    timer.rs           the timer window
    popup.rs           the warning and time-up windows
    draw.rs            Direct2D and DirectWrite setup, colours, fonts
```

## New `windows` crate features

- `Win32_System_Pipes`, `Win32_Storage_FileSystem` — named pipe.
- `Win32_System_Threading`, `Win32_System_Environment` — `CreateProcessAsUserW`, `CreateEnvironmentBlock`.
- `Win32_Graphics_Direct2D`, `Win32_Graphics_Direct2D_Common`, `Win32_Graphics_DirectWrite`, `Win32_Graphics_Dxgi_Common`, `Win32_Graphics_Gdi` — drawing.
- `Win32_UI_HiDpi`, `Win32_UI_Input_KeyboardAndMouse` — DPI and mouse.
- `Win32_System_LibraryLoader` — `GetModuleHandleW` for the window class.

No new crates. `windows-numerics` (already used by `windows`) is named in `Cargo.toml`, because Direct2D points use its `Vector2`.

## Messages

JSON, one message per line. Defined once in `helper_protocol.rs` and used by both sides.

| Message | Fields | When |
|---|---|---|
| `status` | `child_name`, `left_s`, `limit_s`, `reason`, `show_timer` | Every tick, and when the helper connects |
| `warn` | `minutes_left`, `reason` | At 10, 5 and 1 minute |
| `time_up` | `grace_s`, `reason`, `can_ask` | When time is up |
| `extra_time` | `minutes` (below zero when taken), `by` | When a parent gives or takes time |

`reason` is `budget` or `hours`. `can_ask` is true when the child can ask for more time with `/more` in Telegram.

## Steps

Each step ends with something that works and can be tested.

### Step 1: Messages

- `helper_protocol.rs` with the message types and `serde` `tag = "type"`.
- Functions to write one message as a line and to read lines back.
- New optional config field `show_timer` for each child, default `true`.

**Done when:** unit tests pass for each message (write, then read back) and for the new config field.

### Step 2: Pipe server in the service

- `pipe_server.rs`: a thread that creates `\\.\pipe\ScreenControl` instances and waits for clients.
  - The first instance uses `FILE_FLAG_FIRST_PIPE_INSTANCE`. If the name is taken, log an error and run without helpers.
  - Security descriptor from SDDL: SYSTEM full access, interactive users read only.
  - For each client, `GetNamedPipeClientSessionId` gives the session. Only one client per session; a newer one replaces the older.
- The main loop sends `(session, message)` to the pipe thread through a channel. The pipe thread writes to the right client and drops clients that fail.
- A debug command `screen-control helper --print` connects and prints each message. It is used to test this step before any windows exist.

**Done when:** in the VM, `helper --print` in `testkid`'s session prints `status` every tick, and a `warn` at the warnings.

### Step 3: Start the helper

- `launch.rs`: start `screen-control.exe helper` in a session as its user:
  - `WTSQueryUserToken`, then `DuplicateTokenEx` (primary token),
  - `CreateEnvironmentBlock`,
  - `CreateProcessAsUserW` with desktop `winsta0\default` and `CREATE_NO_WINDOW`, so no console window shows.
- The helper allows only one copy per session, with a named mutex `Local\ScreenControlHelper`.
- Each tick, the service starts the helper in each child's session that has no connected helper. Wait at least 10 seconds between tries for the same session, so a broken helper does not start again and again.

**Done when:** in the VM, the helper starts when `testkid` signs in, and starts again within about 20 seconds after it is killed in Task Manager.

### Step 4: Timer window

- `draw.rs`: Direct2D factory, DirectWrite text, colours. Direct2D falls back to software drawing (WARP) when there is no graphics driver.
- `timer.rs`: a small rounded window in a corner that shows the time left, for example `1:25`.
  - Styles: `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED`. It is always on top, not in the taskbar, and does not take focus.
  - Green above 10 minutes, yellow under 10, red under 5.
  - Drag it to move it. It snaps to the nearest corner. Click it to switch between large and small. It cannot be closed.
  - At each warning it goes back to large size.
  - Hidden when `show_timer` is false, and when no `status` came for 90 seconds (the service is gone).
  - Counts down by itself each second between `status` messages, for at most 40 seconds.
- Per-monitor DPI aware (`SetProcessDpiAwarenessContext`), so it is sharp at 100%, 150% and 200%.
- The position and size are saved in the child's own `%LOCALAPPDATA%\ScreenControl\timer.json`. It is only a preference, so the child may change it.

**Done when:** in the VM, the timer counts down with the right colours, can be moved and made smaller, and looks sharp at 100% and 150% display scaling.

### Step 5: Warning and time-up windows

- `popup.rs`: a window in the middle of the screen, shown in front.
  - Warning: the child's name, the minutes left in large numbers, and the reason. It closes after 15 seconds, or with a click or a key.
  - Time-up: "Your time is up" and a countdown to the sign-out. If `can_ask` is true, it also says to send `/more 30` to the bot. It does not close.
  - Extra time: "Mom gave you 30 minutes". It closes after 10 seconds.
- Only one window at a time. A newer one replaces an older one.

**Done when:** in the VM, all three windows show at the right time, and the warnings close by themselves.

### Step 6: The service uses the helper

- In `app.rs`:
  - Each tick, send `status` to the helper of each child's session.
  - `Warn` and `TimeUp` actions: if the session has a connected helper, send `warn` or `time_up` to it. If not, use `WTSSendMessageW` as now.
  - `NotifyPc` from Telegram commands becomes `extra_time` for a session with a helper.
- Log which way each message went (helper or message box).

**Done when:** with the helper running, no message boxes appear. With the helper killed, message boxes appear again until it is back.

### Step 7: Install and test

- The install script needs no change: the helper is the same `.exe`.
- Test in the VM, then on the real PCs:

- [ ] The helper starts at sign-in and after it is killed.
- [ ] No console window shows.
- [ ] The timer counts down and changes colour at 10 and 5 minutes.
- [ ] The timer does not take focus from a game or a text field.
- [ ] Warnings show at 10, 5 and 1 minute and close by themselves.
- [ ] Time-up shows a countdown, then the sign-out happens.
- [ ] `/give` in Telegram shows "… gave you … minutes" on the PC.
- [ ] With the helper killed, the plain message boxes still show.
- [ ] Sharp text at 100% and 150% display scaling.
- [ ] A child cannot use the pipe to change anything: messages only go from the service to the helper.

## Order and size

| Step | Size |
|---|---|
| 1 Messages | Small. |
| 2 Pipe server | Medium. Pipe security needs care. |
| 3 Start the helper | Medium. |
| 4 Timer window | Medium. Most of the drawing work. |
| 5 Warning and time-up windows | Small, reuses the drawing code from step 4. |
| 6 The service uses the helper | Small. |
| 7 Install and test | Small. |

After step 3, nothing changes for the child yet, but the base is tested. After step 6, the feature is complete.
