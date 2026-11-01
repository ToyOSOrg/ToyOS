---
status: open
kind: defect
opened: 2026-11-01
---

# libc_children's tty_interrupt can stall at the nested shell's line

`cargo test --test toyos-build -- libc_children` on the 2-CPU QEMU x86-64 boot
sometimes ends with `tty_interrupt: stalled at the nested line held`.
1. The probe types `shell` into its shell, and the nested prompt prints.
2. It then types `cat` into the nested shell. That line never becomes a held
   foreground line.
3. The probe's own 120 s hang ceiling ends the run with exit 2.

**Evidence.** These are local QEMU runs, with logs in
[`pr-1111/r2/tty`](https://github.com/ToyOSOrg/evidence/tree/8b3306f1ba244133a2c28e8d7c09fcc48c473c8e/pr-1111/r2/tty):

| revision | runs | stalled |
|---|---|---|
| main `24a538c88` | 4 | 1 |
| `f5ae75f6b` (#1111's step 1 on that main) | 6 | 0 |
| `dd9bf974d` (#1111's head) | 3 | 2 |

One of the head's two failures was this stall. The other was
`signal_quit: stalled driving the child that reloads`.

It reproduces on main, so it does not come with #1111. Its cause is unknown.
The kept serial log covers only boot, so nothing yet says which side lost the
line: the nested shell's stdin read, the surface's report of its foreground
line, or a wake between them.

**Exit:** find what the nested shell waits on when it stalls, fix it, and
show `libc_children` green across repeated runs. `signal_quit`'s stall may be
the same defect.
