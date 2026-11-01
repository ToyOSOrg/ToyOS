---
status: open
kind: defect
opened: 2026-11-01
---

# ToyOS std echoes stdin and keeps each line

ToyOS's std reads an interactive stdin in a canonical mode of its own
(`sdk/std/sys/stdio.rs`, `canonical_read`): it echoes each byte to stdout as it
arrives (`echo`), and builds the line in a static buffer (`LINE_BUF`). Every
program that reads stdin through std on a terminal gets this, unless it has
called `std::os::toyos::io::set_stdin_raw(true)`.

- **A secret read through std is shown and kept.** A passphrase typed at a
  terminal appears on screen, and the line stays in `LINE_BUF` after the
  program has taken it, until the next line overwrites it. Nothing wipes it.
  No program can read a secret through std without both happening.
- **Echo is the terminal's.** Whether a byte typed is shown is the terminal
  surface's decision, which a program asks for, not the language runtime's,
  as a terminal's line discipline does on Unix.
  Here std decides inside every program.
- **Worked around** in `system/wifi`'s `passphrase`, which reads descriptor 0
  itself.

- **Owner:** the orchestrator.
- **Exit:** std's stdin echoes nothing and keeps no line of its own. Echo and
  line editing are the terminal's, and a program can ask it not to echo while
  it reads a secret. Once that holds, `wifi` reads through std again and its
  workaround is deleted.
