---
status: open
kind: defect
opened: 2026-10-04
---

# The console says its routine lines as errors

`/system/bin/console` (`userland/console/src/main.rs`), the surface of the
`console/system.toml` boot, writes `console: ready …`, `keyboard layout is
now …`, `client N has the keyboard until it exits` and `client N gave the
keyboard back` with `eprintln!`, and a line on stderr is an Error record
(`toyos::log::stdio::Stream::severity`). So every one of them is drawn in red
on the console and reads as a failure, as the terminal's identical lines did
until they moved to stdout.

Owner: orchestrator. Exit condition: those four lines are written to stdout,
and this file is deleted.
