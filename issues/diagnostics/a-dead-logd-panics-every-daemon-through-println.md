---
status: open
kind: defect
opened: 2026-09-25
---

# A dead logd panics every daemon through println!

Every program init starts writes its stdout and stderr into a pipe whose read
end `logd` holds. If `logd` exits, each of those pipes has no reader, the
next write answers `Gone`, std maps it to an error, and `println!` panics on
an error: every daemon that says a line after `logd` has gone ends — netd,
soundd, sshd, the compositor — and nothing restarts `logd`. The `say!` macros
ignore the refusal, so the daemons that use them survive it; `println!` and
`eprintln!` do not.

## Exit condition

A test that ends `logd` mid-boot finds every other daemon still running and
serving afterwards — by the std PAL answering a write to a pipe with no reader
on slots 1 and 2 without a panic, or by init restarting `logd` on a pipe the
daemons still hold.
