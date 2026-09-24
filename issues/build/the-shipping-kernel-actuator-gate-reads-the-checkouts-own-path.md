---
status: open
kind: tooling
opened: 2026-09-16
---

# The shipping-kernel actuator gate reads the checkout's own path

`assert_actuators_match_features` (`src/build.rs`) asks the artifact whether it
carries an actuator by searching the kernel image for each declared name as a
byte string. The kernel image embeds absolute source paths, so the search also
matches the directory the checkout sits in.

Measured on a worktree made by the documented command,
`cargo run -- --worktree add /Users/jan/Dev/jan/toyos-heartbeat`, at
`dc38a054`:

```
$ cargo run -- --build-only
panicked at src/build.rs:1248:5:
the shipping kernel names 1 of the 111 actuators `kernel/src/actuator.rs`
declares: ["heartbeat"].
$ strings -a target/kernel-42942dede1d2807a | grep -c heartbeat
48
$ strings -a target/kernel-42942dede1d2807a | grep -c 'Dev/jan/toyos-heartbeat'
48
```

All forty-eight hits are the checkout path. No symbol, string or record in the
shipping kernel names the actuator; the gate is reading `toyos-heartbeat` out of
`/Users/jan/Dev/jan/toyos-heartbeat/kernel/src/...`. The test kernel built in
the same tree passes, because its direction of the same assertion wants every
name present.

Two costs, and the second is the one that matters. `cargo run -- --build-only`
is what `CLAUDE.md` tells every agent to build with, and one agent, one worktree
means worktree paths are named after the task — so a task named after any of the
111 declared actuators cannot build the shipping image at all until it renames
its checkout. In the other direction the same aliasing is a hole: a checkout at
a path that happens to contain an actuator name would satisfy the test kernel's
half of the assertion without the kernel carrying the actuator, which is the
failure mode the gate's own doc comment says it exists for ("two builds quietly
becoming one build with test hooks in it").

Not reproduced on CI, which checks out at a path naming no actuator.

**Exit condition**: the gate distinguishes an actuator the kernel carries from
one its file paths spell — searching a section that holds no source paths, or
searching for the name in the form the actuator table actually emits — with a
negative control at a checkout path containing an actuator name.
