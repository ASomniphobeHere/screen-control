# Features

Screen Control limits how long each child can use a Windows 11 PC each day. It replaces Microsoft Family Safety. It runs only on the PC and does not need a cloud service to enforce limits.

## Many children

- The program supports two or more children.
- Each child has a name (for example "Jānis") that is used in messages and in Telegram.
- Each child is linked to their Windows account on each PC.
- Each child has their own limits, allowed hours, used time and extra time.
- One child's time never affects another child's time.
- The program can run on more than one PC. Each PC is shown by its name in Telegram.

## Daily time budget

- Each child has a daily time limit.
- The limit can be different for each day of the week.
- Users without a limit are not controlled.
- Time counts only when the user is signed in and the screen is unlocked.
- Time does not count when the PC sleeps.

## Allowed hours

- You can set hours when use is not allowed, for example after 21:00.
- Outside the allowed hours, the user is signed out.

## Warnings

- The user gets a message 10, 5 and 1 minute before time ends.

## Better warnings (later)

Planned after Telegram and config updates. Until then, warnings are plain Windows message boxes.

- **Timer:** a small timer in a corner of the screen always shows the time left today.
  - Green while more than 10 minutes are left, yellow under 10 minutes, red under 5 minutes.
  - The child can move it to another corner or make it smaller. It cannot be closed. At each warning it comes back to full size.
  - Parents can turn the timer off for a child in the config.
- **Warning window:** at 10, 5 and 1 minute, a window shows the child's name, the time left in large numbers, and the reason (daily limit or allowed hours). It closes by itself after a short time, or with a click.
- **Time-up window:** when time ends, a window shows a countdown to the sign-out. It also says how to ask for more time.
- **Extra time:** when a parent gives or takes time, the child sees it at once, for example "Mom gave you 30 minutes".
- **Backup:** if the timer program is not running, the plain message boxes are used as before.
- **Not a weak point:** if a child closes or kills the timer program, the limits still work. The service starts the timer program again.

## Enforcement

- When time ends, the user is signed out.
- If the user signs in after time has ended, they are signed out again after a few seconds.

## Extra time

- A child can ask for extra time through a Telegram bot, for example `/more 30`.
- The bot knows which child asks, because each child's Telegram account is linked to that child.
- All parents get the request with the child's name, the PC name, the time asked for and the time left today. Example: "Jānis asks for 30 min on PC-1 (5 min left)".
- The request has buttons: Approve, Approve 15 min, Deny. The answer goes only to that child.
- Requests that get no answer expire after some time (for example 30 minutes).
- A parent can give time without a request: `/give <name> <minutes>`.
- A parent can take time away: `/take <name> <minutes>`.
- A parent can see all children: `/status` shows used time, extra time and time left for each child.
- A parent can end a child's time now: `/stop <name>`.
- Only parents can approve, give, take or stop. Children can only ask for time and see their own status.

## Extra time after hours

- Normally, extra time is added to today's budget. Outside the allowed hours this does nothing, because the hours still block use. Just before the hours end, it gives only the minutes until the end.
- So time given when the allowed hours end before the given minutes would run out works differently: it starts at once, on the clock. `/give 30` at 21:40 (hours until 21:00) lets the child use the PC until 22:10. `/give 30` at 20:55 lets the child use it until 21:25.
- At the end, the child is signed out, whether the time was used or not. Time spent signed out, locked or asleep still runs.
- During this time, the daily budget and the allowed hours do not apply. Used time is still counted, for `/status`.
- The same applies to an approved request (`/more`).
- `/give` again during this time makes it longer. `/take` makes it shorter. `/stop` ends it at once.
- It goes on past midnight if needed. Giving after midnight but before the hours start (for example at 06:30, hours from 08:00) works the same way.
- When the hours do not end before the given minutes would run out (for example `/give 30` at 20:00), `/give` works as before.
- The parent's reply says until when: "Done. Jānis can use the PC until 22:10 (outside the allowed hours)."
- The warnings, timer and time-up window work as usual. Their text says "your extra time ends soon" and "your extra time is over".

## Many parents

- The program supports two or more parents. Each parent has a name (for example "Mom").
- All parents have the same rights.
- Every request goes to all parents.
- The first parent to answer decides. The other parents' messages change to show the result, for example "Approved 30 min by Mom". Their buttons stop working.
- When a parent uses `/give`, `/take` or `/stop`, the other parents get a short note about it.
- The child sees which parent answered.
- Messages from unknown Telegram accounts are ignored.
- Extra time is stored apart from the normal budget, so you can see what was given.

## Config updates

- Limits, allowed hours, children and parents are kept in a private GitHub repo.
- To change them, edit the file and push. Each PC gets the change within about 10 minutes. You do not need to touch the PC.
- A parent can send `/reload` in Telegram to get the change at once.
- The parents get a message in Telegram when a PC uses a new config.
- A config with errors is not used. The PC keeps the old one and tells the parents what is wrong.
- Without internet, the PC uses the last config it has.
- Changing the config does not reset the time already used today.

## Protection against cheating

- Children cannot stop, change or remove the program.
- Children cannot see or edit the usage data.
- Changing the time zone or the clock does not reset the day or give more time.
- The program also runs in Safe Mode.
- The program restarts itself if it crashes.

## Logging

- The program writes a log of every check: child, session, locked or not, time added and time left.
- The log also records every request, approval and grant, with the child's name and the parent's name.
- When something goes wrong, the log shows why.
