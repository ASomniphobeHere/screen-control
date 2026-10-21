# Screen Control

A Windows 11 service that limits children's daily screen time. It replaces Microsoft Family Safety.
Parents control it from Telegram.

## What it does

- Each child has a daily time limit, and optional allowed hours (for example `08:00-21:00`).
- Time counts only while the child is signed in and the screen is not locked.
- A timer in the corner of the screen shows the time left. It is green, then yellow under 10 minutes,
  then red under 5 minutes.
- Warnings show at 10, 5 and 1 minute. When time is up, the child is signed out after 10 seconds.
- Parents use a Telegram bot to give or take time, stop a child's time, and see the status.
- A child can ask for more time with `/more 30`. The parents get a message with buttons to approve or deny.
- The config can be changed from a private GitHub repo. The PC gets the change within 10 minutes.

## How it works

- The service runs as `LocalSystem`, so a standard (non-admin) user cannot stop it or change its files.
- If it is killed, Windows starts it again. It also runs in Safe Mode.
- It starts a small helper program in each child's session. The helper draws the timer and the warning
  windows. If the helper is not running, the service shows plain message boxes. The limits never
  depend on the helper.
- It only makes outgoing connections (Telegram and GitHub). No ports are opened.
- Each PC has its own Telegram bot.

## Requirements

- Windows 11, with the children on **standard** user accounts (not administrators).
- A Telegram bot for each PC, made with [@BotFather](https://t.me/BotFather).
- Optional: a private GitHub repo for the configs, and a fine-grained GitHub token that can only read it.
- To build: Rust (stable, MSVC target). The program has no other runtime files.

## Build

```powershell
cargo build --release
```

The program is `target\release\screen-control.exe`.

## Config

One JSON file per PC. See [`example-config.json`](example-config.json).

```json
{
  "pc_name": "PC-1",
  "utc_offset_minutes": 180,
  "parents": [
    { "name": "Mom", "telegram_user_id": 111111111 }
  ],
  "children": [
    {
      "id": "janis",
      "name": "Jānis",
      "sid": "S-1-5-21-1111111111-2222222222-3333333333-1001",
      "telegram_user_id": 333333333,
      "limits": { "default": 120, "sat": 240, "sun": 240 },
      "allowed_hours": "08:00-21:00",
      "show_timer": true
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `pc_name` | Must match the PC name given at install. |
| `utc_offset_minutes` | Local time minus UTC, in minutes. The day starts at local midnight. |
| `parents` | People who can control the PC from Telegram. |
| `telegram_user_id` | A number. Send any message to [@userinfobot](https://t.me/userinfobot) to get yours. |
| `children[].id` | Short name used in Telegram commands, for example `/give janis 30`. |
| `children[].sid` | The child's Windows account SID. Get it on the PC with `whoami /user` while signed in as the child. |
| `children[].telegram_user_id` | Optional. Lets the child use `/more` and `/status`. |
| `limits` | Minutes per day. `default`, plus optional `mon` … `sun`. |
| `allowed_hours` | Optional. Outside these hours the child is signed out. |
| `show_timer` | Optional, default `true`. `false` hides the corner timer. Warnings still show. |

Check a config file before you use it:

```powershell
.\screen-control.exe check-config .\pc-1.json --pc-name PC-1
```

## Install

Run PowerShell **as Administrator** on the child's PC. Put `install.ps1` and `screen-control.exe` in the
same folder, or run it from the repo after a build.

### Install script format

```powershell
.\scripts\install.ps1 [-ExePath <path>] [-ConfigFile <path>] [-PcName <name>]
                      [-BotToken <token>] [-GitHubToken <token>] [-GitHubRepo <owner/repo>]
                      [-CheckTelegram]
```

| Parameter | Meaning |
|---|---|
| `-ExePath` | The program to install. Default: `screen-control.exe` next to the script, then `..\target\release\`. |
| `-ConfigFile` | A local config file. If not given, the config is downloaded from the config repo. |
| `-PcName` | The PC name in the config. Default: the Windows computer name. |
| `-BotToken` | The Telegram bot token from BotFather. |
| `-GitHubToken` | Read-only token for the config repo. |
| `-GitHubRepo` | The config repo, as `owner/repo`. The script reads `pcs/<PcName>.json` from it. |
| `-CheckTelegram` | Install nothing. Only show the bot link and send each parent a test message again. |

On the first install, if you give none of `-PcName`, `-BotToken`, `-GitHubToken` and `-GitHubRepo`,
the script asks for them. Typing the tokens at the prompt keeps them out of your PowerShell history.

### Examples

```powershell
# First install with a local config file. The script asks for the secrets.
.\install.ps1 -ConfigFile .\pc-1.json

# First install from the config repo.
.\install.ps1 -PcName PC-1 -GitHubRepo owner/screen-control-config

# Update the program only. Secrets and config are kept.
.\install.ps1 -ExePath .\screen-control.exe

# Check again who can get Telegram messages.
.\install.ps1 -CheckTelegram
```

If the script is blocked by the execution policy:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -ConfigFile .\pc-1.json
```

### What the install does

1. Makes `C:\ProgramData\ScreenTime`. Only SYSTEM and Administrators can open it.
2. Saves `secrets.json` there (PC name, bot token, GitHub token and repo). Secrets stay only on the PC.
3. Gets the config and checks it with the new program. If it has errors, nothing is changed.
4. Copies the program to `C:\Program Files\ScreenControl`, then makes and starts the `ScreenControl`
   service, with restart on failure and Safe Mode support.
5. Prints the bot link (`https://t.me/<bot>`) and sends each parent "Screen Control is installed on
   <PC name>." It lists the parents who were not reached.

Running the script again updates the program. Saved time, secrets and config are kept unless you give new
values.

### After the install

- **Each parent** (and each child with a Telegram ID) must open the bot link and press **Start**.
  A bot cannot send messages to a person who has not done this.
- Then run `.\install.ps1 -CheckTelegram` until every parent shows OK.
- Sign in as the child and check that the timer shows.

## Uninstall

```powershell
.\scripts\uninstall.ps1              # keeps the config, secrets, saved time and logs
.\scripts\uninstall.ps1 -RemoveData  # also deletes C:\ProgramData\ScreenTime
```

## Telegram commands

Parents:

| Command | What it does |
|---|---|
| `/status` | Used time, extra time and time left for each child. |
| `/give <name> <minutes>` | Give extra time today. |
| `/take <name> <minutes>` | Take time away today. |
| `/stop <name>` | End the child's time now. |
| `/reload` | Get the config from the config repo now. |

If the PC has only one child, the name can be left out, for example `/give 30`.
When one parent gives, takes or stops time, the other parents get a short note.

Extra time after hours: if the allowed hours end before the given time would run out, the time
starts at once and runs on the clock. For example, `/give 30` at 21:40 (hours until 21:00) lets
the child use the PC until 22:10, and `/give 30` at 20:55 until 21:25. During this time the
daily limit and the allowed hours do not apply. At the end the child is signed out, even if the
time was not used. `/give` again makes it longer, `/take` makes it shorter and `/stop` ends it.
The same applies to an approved `/more` request. A child without allowed hours always gets the
time added to today's limit.

Children:

| Command | What it does |
|---|---|
| `/more <minutes>` | Ask the parents for more time. A request ends after 30 minutes. |
| `/status` | Your time left today. |

## Config repo

- A private repo with one file per PC: `pcs/<PC name>.json`.
- The service checks it every 10 minutes and on `/reload`. Parents get a message that lists the changes.
- A config with errors is not used. The service keeps the last good config (`config.last-good.json`).
- **Do not put tokens in the config repo.** Tokens are only in `secrets.json` on each PC.

## Files on the PC

| Path | What |
|---|---|
| `C:\Program Files\ScreenControl\screen-control.exe` | The program (service and helper). |
| `C:\ProgramData\ScreenTime\config.json` | The config in use. |
| `C:\ProgramData\ScreenTime\config.last-good.json` | Backup of the last good config. |
| `C:\ProgramData\ScreenTime\secrets.json` | Tokens. Only SYSTEM and Administrators can read it. |
| `C:\ProgramData\ScreenTime\state.json` | Time used today, extra time, passes for extra time after hours, open requests. |
| `C:\ProgramData\ScreenTime\logs\` | Daily service logs. |
| `%LOCALAPPDATA%\ScreenControl\` | The child's timer position and the helper log. |

## Program commands

```text
screen-control service [--data-dir DIR]            run as a Windows service (started by Windows)
screen-control console [--data-dir DIR]            run in this terminal, for testing
screen-control check-config FILE [--pc-name NAME]  check a config file
screen-control helper                              the helper in each child's session
screen-control helper --show POPUP...              show sample popups (warn10, warn5, warn1,
                                                   warn-extra, time-up, time-up-hours,
                                                   time-up-extra, extra, taken)
```

## Known limits

- A child with an administrator account can remove the service. Use standard accounts.
- The day is taken from the PC clock. Moving the clock back does not give more time, but moving it to
  the next day starts a new day. Standard accounts cannot change the clock.
- A child can hide the timer and warning windows (for example by running a copy of the program first).
  The warnings then show as plain message boxes, and the limits still apply.
- The time-up countdown is 10 seconds. Unsaved work can be lost when the child is signed out.

## More

- [`specs/features.md`](specs/features.md): all features.
- [`specs/technical-requirements.md`](specs/technical-requirements.md): the technical design.
- [`plans/`](plans/): implementation plans and test checklists.
