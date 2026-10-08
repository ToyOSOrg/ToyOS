---
status: open
kind: tooling
opened: 2026-10-03
---

# The metal loop speaks on stdout, and none of its lines says when

Every statement the build system and the harness make goes through
`src/printer.rs`'s `eprintln!` and opens with the UTC time of day it was made
at. `toyos-metal` narrates a run with `println!` and `print!` instead, and
stdout passes through no printer:

```
rg -n '(^|[^e])print(ln)?!' src/metal.rs src/bin/toyos-metal.rs
```

Its progress is among them — `  run: <command>`, `machine <vendor> <product>,
BIOS <version>`, `the machine answered ssh again after N s`, `the boot stick
enumerated N s after the machine answered`, `readback written to <dir>` — so a flash, a boot and a readback
are the one stretch of a run whose lines cannot be laid beside the suite's or
beside `/log` by time. The harness's own `[metal]` lines around them are
stamped.

Not all of them are speech. `print!("{loader}{log}")` relays what the machine
wrote, which carries the machine's own stamps, and `PASS: the machine booted ToyOS in N ms` is the
loop's verdict.

Owner: the orchestrator, which alone runs the loop against the machine and so
is the only reader who can see a change to it.

**Exit**: every line `toyos-metal` says about its own progress opens with the
time of day, as a line of the suite does, and what stays on stdout is what a
caller of the loop reads.
