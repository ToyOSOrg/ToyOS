---
status: open
kind: defect
opened: 2026-11-05
---

# A desktop reopened as its session ends is handed the ended session

When the desktop's session ends, the compositor asks the supervisor for a new
one at once (`system/compositor/src/signout.rs`). The supervisor answers the
screen's row with its open desktop session, one whose owner it does not yet
see as ended (`system/supervisor/src/sessions.rs`, `serve_login` through
`desktop_session` and `exit_of`). An ask that lands between the owner's exit
and the supervisor reading it is handed the ended session, whose control port
then answers `Gone`; the compositor says `no desktop session: … Gone; the next
launch asks again`, and opens none until something launches.

Seen once, in guest rows run 42089538985 (`session_lifecycle`, QEMU x86-64,
`tests/images/sessioncase.toml`), which stalled at "the desktop to open
again": the session owner exited at 4.967 s, the supervisor said `session 8 is
signed out` at 4.973 s, and the compositor's refusal is stamped 4.973 s. The
row passed at that branch in runs 42018194698, 42040884795 and 42080820355.

**Exit**: an ask the supervisor answers is never an ended session's, or the
compositor asks again when the one it was handed is gone; a host test of the
supervisor's decision with the owner's exit not yet read holds it.
