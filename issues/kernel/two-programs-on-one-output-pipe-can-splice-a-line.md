---
status: open
kind: defect
opened: 2026-09-24
---

# Two programs writing one output pipe can splice a line written in two writes

A program's stdout and stderr are one pipe to `logd`, made by init for each
program it starts, and every child that program spawns inherits it
(`userland/init`). `logd` ends a line at the pipe's next newline, so a line one
writer puts into the pipe in two `write`s — `println!` does, when its buffer
already held part of a line — can have another writer's bytes land between its
halves. Before, each process's stdout was a console object minted for it at
spawn, which buffered its line and emitted it whole (`console_line_atomicity`,
which now measures the console object through each writer's slot 0).

Not run: the reading of the two paths. `test-runner`'s children are the case
the harness has: the C family's capture compares a test's output line by line.

## Exit condition

A line one process writes in several `write`s reaches `logd` whole whatever
another process holding the same pipe writes meanwhile — or a test that has two
processes share one output pipe, each writing every line in two `write`s,
finds no line carrying both writers' bytes in `/log`.
