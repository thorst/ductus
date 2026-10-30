# Event Log IDs

On Windows, the service writes what operations teams watch for to the Application log, source `Ductus`, with these IDs; Event Viewer shows the text below. Everything else goes only to the log files in `C:\ProgramData\Ductus\logs`. Errors not listed here are event 1100, written at most once a minute for each kind of message, so the log can't flood. On Linux, the same lines are in the journal (`journalctl -u ductus`).

## 1000 Â· Information

The engine started and is taking messages: its version, and the start's details (interfaces, where the API listens).

> Ductus â€¦ started.

## 1001 Â· Information

The service stopped cleanly (a stop, a shutdown, or a drain that ended with a stop): every message in hand was finished first.

> Ductus stopped cleanly: it finished the messages in hand.

## 1002 Â· Warning

The service stopped before finishing the messages in hand (the stop timeout passed, or a second stop came). Nothing is lost: they're pending and are processed again at the next start, so a destination may get one twice. If it recurs, look at what was slow (a receiver timing out) in the log files.

> Ductus stopped before it finished the messages in hand: they stay pending and are processed again at the next start (a destination may get one again).

## 1003 Â· Error

The engine couldn't start, and the service stays stopped: the text says why (the database can't be reached, the project has an error, a certificate can't be used, another engine runs some of these interfaces). Fix the cause and start the service.

> Ductus could not start. The service stays stopped until the cause is fixed and it is started again.

## 1004 Â· Warning

The previous run didn't stop cleanly (killed, crashed, or the machine lost power). The messages it had in hand are processed again; the previous run's last lines are in the log files.

> The last run of Ductus on this computer did not stop cleanly (it was killed, crashed or lost power). The messages it had in hand are processed again, as the delivery guarantee says.

## 1005 Â· Warning

The service runs as an account with a password, which an install or upgrade can't keep, so it was left stopped. Run `ductus service account <account> --start` as an administrator and type the password; a gMSA needs none of this.

> The Ductus service runs as â€¦, an account with a password, which an install or upgrade can't keep: the service was left stopped. As an administrator, run

## 1006 Â· Error

The engine stopped on an error while running: the text says which.

> Ductus stopped on an error.

## 1007 Â· Information

The service's account was changed with `ductus service account`: from which, to which, and by whom.

> The Ductus service's account was changed.

## 1010 Â· Error

An interface's code was stopped: it ran past its `timeout` three times in a row, or out of memory. Its messages wait; fix the code, deploy it and restart the service.

> An interface was stopped: its code ran away. Its messages wait until the code is fixed and the engine is restarted.

## 1020 Â· Error

A critical alert fired or escalated (the alert channels are told too), for tools that watch the Event Log.

> A critical alert fired.

## 1100 Â· Error

Any other error the engine logged, at most once a minute for each kind of message and what it's about; the log files have every line.

> Ductus logged an error.

