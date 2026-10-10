---
status: open
kind: defect
opened: 2026-10-10
---

# A question waits unseen while a file is picked, and every desktop launch waits with it

`/system/bin/filepicker` serves both a program's file-picking session
(`filepicker`) and the supervisor's question before a package's launch
(`consent`), from one loop. A picking session is `run_picker`, called inside
that loop, which returns only when the session ends. A question asked during
one stays queued on the `consent` acceptor, unseen, until the session ends.

The supervisor holds that launch parked until the question is answered, which
is right. But the compositor makes every launch on one thread, one at a time
(`userland/compositor/src/session.rs`, `launcher()`), because a launch that
asks waits as long as its question. So while the question waits unseen, no
launch from the desktop is made at all, the terminal's chord included, until
the person closes the picker, whose window gives no sign of why.

**Evidence**: read from the code at the branch that made the question
(`userland/filepicker/src/main.rs`, the `run_picker` call in `main`'s loop;
the compositor's `launcher`). Not run: no test opens a picker and launches a
package while it is open.

## Owner

`userland/filepicker` and the compositor's launch path, in the desktop track
`issues/toyos-has-a-desktop.md`.

## Exit condition

A guest test opens a file-picking session, launches a package that asks from
the desktop, and sees the prompt on the panel while the picker is still open;
and a launch from the desktop that asks nothing starts while another's
question is up.
