---
status: open
kind: defect
opened: 2026-10-13
---

# Files acts on a key's release as on its press

`apps/files/src/main.rs`'s `Event::KeyInput` arm matches `ev.keycode` on every
transition and never asks `pressed()`. A key's release carries its usage too,
so one press and its release do the action twice: an arrow moves the selection
two places, Backspace goes up two directories, and Enter opens the selected
item twice.

Found by reading, in the review of ToyOSOrg/ToyOS#911, which fixed the same
shape in the terminal's copy chord; not reproduced on a guest.

**Exit:** a release reaches none of those actions.
