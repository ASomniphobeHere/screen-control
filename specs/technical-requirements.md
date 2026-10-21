# Technical Requirements

## Language and libraries

- Rust, compiled for Windows (x86_64-pc-windows-msvc).
- Crates:
  - `windows-service` — run as a Windows service.
  - `windows` — Win32 API. Features: `Win32_System_RemoteDesktop`, `Win32_System_WindowsProgramming`, `Win32_Security`.
  - `serde` and `serde_json` — config and state files.
  - `chrono` — dates and week days.
  - `reqwest` or `teloxide` — Telegram Bot API.

## Service

- Runs as a Windows service under the LocalSystem account (session 0).
- Starts automatically when Windows starts.
- Accepts session change events (`SERVICE_ACCEPT_SESSIONCHANGE`): logon, logoff, lock, unlock. These give a fast reaction.
- A loop runs every 15–30 seconds. The loop is the main source of truth, because events can be missed (sleep, user switching, crashes).

## Counting time

- `WTSEnumerateSessionsW` — list sessions. Use only sessions in the `WTSActive` state.
- `WTSQuerySessionInformationW`:
  - `WTSUserName` — the user of the session.
  - `WTSSessionInfoEx` — whether the session is locked. Locked sessions do not count.
- `QueryUnbiasedInterruptTime` — measure time between checks. It does not count sleep time.
- Limit each time step to about 2× the loop interval.
- Identify users by SID, not by name.

## Day change

- Find the date from UTC time plus a fixed offset in the config. Do not use the system time zone.
- Save the last seen UTC time. If the clock goes back, continue the current day. Do not reset it.

## Enforcement

- `WTSSendMessageW` — show warning messages in the user's session from the service.
- `WTSLogoffSession` — sign out the user when time ends.

## Helper program for better warnings (later)

The service runs in session 0 and cannot show its own windows on the child's screen. A helper program in the child's session shows the timer and the warning windows. The service still does all counting and sign-outs. The helper only shows what the service tells it.

### Starting the helper

- Same `.exe` with a new command: `screen-control helper`. Nothing more to install.
- The service starts the helper in each child's session, as the child's user:
  - `WTSQueryUserToken` — the child's token,
  - `DuplicateTokenEx` — a primary token,
  - `CreateEnvironmentBlock` — the child's environment,
  - `CreateProcessAsUserW` with desktop `winsta0\default`.
- Started at sign-in, and when the service starts with a child already signed in.
- Each tick, the service checks that the helper is running in each child's session, and starts it again if not.
- The helper ends by itself when the session ends.

### Talking to the helper

- Named pipe `\\.\pipe\ScreenControl`, created by the service at start with `FILE_FLAG_FIRST_PIPE_INSTANCE`. The service starts before anyone signs in, so no other program can take the name first.
- Pipe access (ACL): SYSTEM full, signed-in users connect only.
- The service checks which session a client is in with `GetNamedPipeClientSessionId`, and only sends a session its own child's data.
- Messages are JSON, one per line, from the service to the helper:
  - `status` — child name, time left, limit, reason and `show_timer`, every tick,
  - `warn` — minutes left and reason,
  - `time_up` — seconds until sign-out, reason, and if the child can ask for more time,
  - `extra_time` — minutes given or taken, and by which parent.
- The helper sends nothing back in this version.
- The service only uses a client that runs its own `.exe` (`GetNamedPipeClientProcessId`, `QueryFullProcessImageNameW`). Other clients are disconnected, so a fake helper cannot take the place of the real one and hide the warnings. Users cannot write to the install folder.
- While a helper is connected for a session, the service does not send message boxes to that session. If not, it uses `WTSSendMessageW` as before.

### Windows

- Drawn with plain Win32 windows, Direct2D and DirectWrite through the `windows` crate. No extra runtime is needed.
- Not OpenGL-based GUI libraries: PCs and VMs without a good graphics driver may not run them. Direct2D falls back to software drawing (WARP).
- Per-monitor DPI aware, so text is sharp on all screens.
- Timer window: `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED` (see-through edges). Always on top, not in the taskbar, does not take focus from games or typing.
- Warning and time-up windows: on top and shown in front.
- Known limit: games in exclusive full-screen mode can cover all windows, including message boxes.

### Config

- New optional child field `show_timer` (default `true`).

## Data storage

- Folder: `C:\ProgramData\ScreenTime\`.
- Folder access (ACL): only SYSTEM and Administrators. Users have no read access.
- `secrets.json` — the Telegram bot token and the GitHub token. Only on this PC, never in the repo.
- `config.json` — PC name, UTC offset, parents, and a list of children. It is a copy of the file from the config repo (see "Config updates"). Each child has:
  - an ID and a name,
  - the Windows account SID on this PC,
  - their Telegram user ID,
  - limits in minutes for each week day (`mon` … `sun`), with `default` for days not set. Every day must have a limit,
  - allowed hours, for example `08:00-21:00` (optional: without it, any hour is allowed),
  - `show_timer` — show the timer window (optional, default `true`).
- `state.json` — for each child: today's used time and extra time. Also open extra time requests and the last seen UTC time.
- Example `config.json`:

  ```json
  {
    "pc_name": "PC-1",
    "utc_offset_minutes": 180,
    "parents": [
      { "name": "Mom", "telegram_user_id": 111111 },
      { "name": "Dad", "telegram_user_id": 333333 }
    ],
    "children": [
      {
        "id": "janis",
        "name": "Jānis",
        "sid": "S-1-5-21-...-1001",
        "telegram_user_id": 222222,
        "limits": { "default": 120, "sat": 240, "sun": 240 },
        "allowed_hours": "08:00-21:00"
      }
    ]
  }
  ```
- Write files safely: write to a temp file, then rename it.
- Log file with rotation.

## Telegram bot

- The service asks the Telegram Bot API for new messages (long polling). Connections go out only. No open ports are needed.
- The bot token is in `secrets.json`. The list of parents (name and Telegram user ID) is in `config.json`.
- Find the child by the Telegram user ID of the sender (`message.from.id`), not by the text of the message. A child cannot ask for time for another child.
- Accept approve, give, take and stop commands only from user IDs in the parents list. Check the ID on button presses too (`callback_query.from.id`).
- Ignore messages from unknown user IDs, and log them.
- Each request gets a short ID. Approve and Deny buttons use inline keyboard callback data such as `approve:<request_id>:<minutes>`, so each button is tied to one child and one request.
- Send each request to every parent. Save the message ID of each copy in `state.json` with the request, so all copies can be edited later.
- A request can be answered only once. The first answer wins. Later button presses get a short reply ("Already approved by Mom") through `answerCallbackQuery`.
- After a parent answers, edit all parents' copies to show the result and the parent's name, remove the buttons, and send the answer to the child.
- Expired requests are removed, and their messages are edited to show "Expired".
- A bot can only send messages to a user who has started a chat with it. Each parent and child must press Start in the bot once during setup.
- Names in `/give <name>` are matched to child IDs or names without case.

### More than one PC

- Telegram allows only one program to read updates from one bot at a time. Two PCs on the same bot would conflict.
- Simple solution: one bot for each PC. Each bot has its own token, and the PC name is shown in each message.
- Each PC counts time on its own. A child who uses two PCs has a separate budget on each. A shared budget for all PCs is not in the first version.

## Config updates

- Config files are kept in a separate **private** GitHub repo, not in the code repo.
- One file for each PC: `pcs/<pc_name>.json`. Each PC has its own SIDs, so the files are different.
- The service downloads its file through the GitHub REST API (`GET /repos/{owner}/{repo}/contents/pcs/<pc_name>.json`). Git does not need to be installed.
- Access uses a fine-grained GitHub token with **read-only** access to this one repo. The token is in `secrets.json`.
- The service checks for a new version every 10 minutes, at start, and when a parent sends `/reload`. It compares the file's SHA, so it downloads only when the file changed.
- Before using a new config, the service checks it:
  - valid JSON with all required fields,
  - limits and times in a valid range,
  - `pc_name` matches this PC,
  - at least one parent.
- If the check passes: save it as `config.json` (temp file, then rename) and use it at once. No restart is needed.
- If the check fails: keep the old config, log the error, and tell the parents in Telegram.
- If GitHub cannot be reached: keep using the local `config.json`. Time limits work without internet.
- Each config that passes the checks is also saved as `config.last-good.json`.
- At start, the service uses the first config that works: `config.json`, then the config repo, then `config.last-good.json`. Without a good config nothing is limited, so all three are tried.
- After an update, tell the parents in Telegram, with the short commit SHA. Example: "PC-1: config updated (a1b2c3d)".
- A config change does not reset today's used time or extra time. A new limit is used from the next check.
- Log every update: old SHA, new SHA, and what changed.
- Anyone who can push to the config repo controls the limits. Only parents get write access. The children must not have access.

## Installation

One PowerShell script, run as Administrator, that:

- Copies the program file.
- Creates the service with `New-Service`, automatic start.
- Sets restart on failure with `sc failure`.
- Adds the service to `HKLM\SYSTEM\CurrentControlSet\Control\SafeBoot\Minimal` and `...\SafeBoot\Network`, so it runs in Safe Mode.
- Creates the data folder and sets its ACL.
- Asks for the PC name, the bot token and the GitHub token, and writes `secrets.json`.
- Downloads the first config from the config repo.
- Shows the bot link (`https://t.me/<bot username>`, from `getMe`) and sends a test message to each parent. Prints which parents still must press Start. Telegram problems only print warnings; the install does not fail.
- `install.ps1 -CheckTelegram` does only this check again, without installing. It reads the token from `secrets.json`, so it needs Administrator. The token is never printed.

## PC setup (done by hand)

- Children use local standard accounts, not administrator accounts.
- Children do not know any administrator password.
- Remove Microsoft Family Safety from their accounts.
- Block time zone changes with Group Policy (optional when the fixed UTC offset is used).
- Set a BIOS/UEFI password and lock the boot order, so the PC cannot start from USB.
- Remove other accounts that the children can sign in to.

## Testing

- Test the counting and day-change logic with unit tests.
- Test with two or more children: time and grants for one child must not change another child's data.
- Test that a child cannot approve requests or give time.
- Test with two parents: when both press a button, only the first answer counts.
- Test config updates: a bad config is not used, and an update does not reset today's time.
- Test the Windows API parts on a spare local standard account.
