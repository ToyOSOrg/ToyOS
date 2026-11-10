---
status: open
kind: finding
opened: 2026-11-05
severity: [hang]
---

# The console waits on logkeeper's file writes

`logkeeper`'s loop is the only thing that reads the rings and grows the
replay the console's carriers and readers are fed from, and it makes every
call on the LOG file server itself, each one blocking (`system/logkeeper/src`):

- a round's write and sync (`Log::to_volume`: `Volume::write`, `Volume::sync`);
- the catch-up of a file behind the replay (`Log::caught_up`, from a round and
  from `Log::adopt`: `Volume::catch_up`'s reopen, writes, sync and rotation);
- a rotation's sync, retention sweep and create (`Log::rotate_if_full`);
- the stop's catch-up and sync (`Volume::finish`), whose time bounds how many
  calls it starts and not how long one takes;
- the stop mark and its removal (`store::mark_stopped`, `unmark_stopped`).

`LOG_WRITE_BUDGET` judges a call only once it returns. So a LOG server that
takes a call and never answers, alive but wedged, leaves every console channel
silent for as long as it does, with no failure bound, and the supervisor
restarts only a server that ended. The stop's wait on the volume's opener is
the one wait here with a bound (`STOP_CATCH_UP`).

Read from the code, not reproduced. The volume's open waited the same way
before it moved to a thread of its own, which is how the console stood silent
for the supervisor's 30 s declaration bound on `acpi_press_is_one_sci`'s boot.

**Exit**: no console channel waits on a `/log` call; a guest boot whose LOG
server stops answering mid-boot still carries its later lines. The shape: the
file is a consumer of the replay with its own offset, written on its own
thread as the readers and carriers are, and the stop asks it to reach the
replay's end and sync within a bound. The per-file offset and the stop's
bounded catch-up are in the tree (`Volume::through`, `catch_up`, `finish`).
What is left is the move: the loop still owns the `Volume` and makes the calls
above, so the volume goes to a thread of its own that reads the replay, and
the stop's bound becomes a wait on that thread, which the loop can give up on.

Owner: `system/logkeeper`; unheld.
