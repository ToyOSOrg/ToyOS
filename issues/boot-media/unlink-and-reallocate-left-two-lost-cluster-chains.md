---
status: open
kind: defect
opened: 2026-08-27
---

# `fat_backing_revoked` left the log volume with two clusters no directory entry reaches

Seen once on 2026-08-27, dev host, full fast tier twelve wide with a second
worktree's suite on the machine, on `wt/toyos-md1` at `03af5421` — a branch
touching no kernel byte and nothing under `toyos-fat32/`:

```
FAIL fat_backing_revoked: the unlink-and-reallocate cycle left the log volume
breaking the format:
1 cluster(s) from 44 are marked allocated and no directory entry reaches them
1 cluster(s) from 137 are marked allocated and no directory entry reaches them
```

**This is a content verdict, not a wall-clock guard.** The judge is
`toyos-fat32-check`, the outside FAT implementation built from Microsoft's
`fatgen103` — so unlike every `ALONE: GREEN` entry whose red is the thing the
test was going to assert, what this says is that the bytes on the volume were
wrong when the machine stopped. Two lost chains, each one cluster long, each
from a different start.

Alone on the same tree minutes later: **green, 5 s.** So the observation is one
red beside eleven other guests and no rate. `cargo run -- --known-red
fat_backing_revoked` answers `NOT ON THE LIST`.

What the test does is unlink a file out from under a held descriptor and let the
next writer take its clusters, then read the volume back off the image after a
shutdown. Two clusters marked allocated and unreachable is the shape of a chain
freed in the FAT and not in the directory, or a directory entry rewritten before
its old chain was released — which of those it is nobody has looked at.

What is owed first is the rate, measured in one session against an unchanged
tree, and then which side of the unlink lost them. The image is the evidence and
the checker is the instrument, so a reproduction keeps its own artifact: unlike
a timing red, this one leaves bytes behind.
