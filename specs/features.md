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
