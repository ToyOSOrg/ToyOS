---
status: open
kind: defect
opened: 2026-08-27
---

# `syscall_window_nmi` lands 0 of 2,427 NMIs in the window beside other guests, and passes alone

Seen once on 2026-08-27, dev host, full fast tier twelve wide with a second
worktree's suite on the machine, on `wt/toyos-md1` at `03af5421`:

```
  [nmi-window] 3000 sent, 2427 taken, 0 in the window, 77 in Ring 3, 18 syscalls made under the storm
FAIL syscall_window_nmi: 3000 NMIs were sent and 2427 taken under TCG, and not
one landed in the syscall window — this accelerator delivers at translation-block
boundaries and `syscall` ends one, so the instrument proved nothing about the
stack the CPU pushes on
```

Alone on the same tree minutes later: **green, 6 s.** So the window is reachable
under TCG and the test's own sentence is about this run rather than about the
accelerator — beside eleven other guests the guest made 18 syscalls under a
3,000-NMI storm and none of them coincided.

**The verdict is honest and that is why this is filed rather than re-run.** The
test refuses to report a pass on a run where its instrument proved nothing,
which is the right behaviour and the opposite of the silently-inert arm this
suite treats as its worst defect class. What it has no answer for is a host that
makes the coincidence too rare: 18 syscalls in ten seconds is a guest getting a
twelfth of the machine, and the storm is sized for a guest that is running.

`cargo run -- --known-red syscall_window_nmi` answers `NOT ON THE LIST`.

Two shapes, and they are not equivalent. Pace the storm against the guest's own
syscall count rather than against a fixed 3,000, so a starved guest takes longer
instead of proving nothing; or keep the count and let the verdict be the
declared degradation the way `screen_fatal_halt_composited` does, which needs a
rate first. The rate is what is owed either way, and it needs one session against
an unchanged tree.
