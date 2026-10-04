---
status: open
kind: defect
opened: 2026-10-04
---

# The compositor says its routine lines as errors

The compositor writes its status with `eprintln!` (`userland/compositor/src`),
and `toyos::log::stdio` records a line written to stderr at `Severity::Error`.
So `compositor: ready`, its wallpaper size and its periodic `frames=…` census
are Error records. Since the console and `cargo run`'s terminal draw an Error
record's text in bright red, every one of them reads as a failure: a
`cargo run` boot on macOS under TCG drew 22 compositor lines, all
`{… error compositor}` in red, and none of them was one.

Owner: orchestrator. Exit condition: the compositor's routine lines are
recorded at Info (`toyos::log`'s `say!` or stdout), a boot's console carries
no `error compositor` line that reports no failure, and this file is deleted.
