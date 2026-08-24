---
status: open
kind: defect
opened: 2026-08-03
---

# A machine with no console says nothing between `Boot: complete` and the terminal

**Measured under metal-sim, and worse than "no scrollback".** With
`--metal-sim --mute` and no virtio-console the guest has no output channel at
all once the last boot checkpoint has painted: the failure screen ends at
`Boot: complete`, and soundd's null-sink line and netd's exit line — printed seconds later,
and read directly off the console by `metal_sim_compositor` on the same machine
shape with the 16550 on — reach no pixel and no file. A running ToyOS on the
T14 is mute between `Boot: complete` and the moment the compositor's terminal
exists. That is fine for a first boot and not fine for debugging input on the
machine. It is also the entire cost the mute default was buying, which is why
the metal-sim profile now keeps its 16550 by default.

Narrowed but not closed: the last checkpoint now paints where this boot's log
can be read (`main.rs`'s `report_log_destination`), so the panel says whether
there will be anything to go back to. What it cannot give that machine is a
line *after* the checkpoint.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling). It is measured
rather than suspected, and what it costs is the window in which input debugging
on real hardware happens: a T14 running ToyOS says nothing between
`Boot: complete` and the moment the compositor's terminal exists. Owed by
whoever gives that window an output channel; `report_log_destination` narrowed
it by saying where the log will be, and cannot put a line in it.
