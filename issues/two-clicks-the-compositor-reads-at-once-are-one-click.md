---
status: open
kind: defect
opened: 2026-10-10
---

# Two clicks the compositor reads at once are one click

`toyos_desktop::fold_mouse` folds a whole read of the mouse claim into one
sample: whether the left button went down and came up somewhere in it, and the
last position. Two clicks in one read, at two places, are therefore one press
and one release, both at the second click's position, and the first click is
lost. A person clicking `+` and then a launcher row quickly enough for one
read to carry both, which a loaded machine makes likelier, has the row's
click land on a closed launcher.

`consent_prompt` (`tests/toyos.rs`) met it on a loaded host (load average
57 when its batch of runs ended): it clicked `+` and then the launcher row
with nothing between them, and no launch was made, which these two clicks
folded into one on the row of a launcher not yet open accounts for. The test
now waits for the launcher on the panel before its second click.

## Owner

The compositor's input decisions, `userland/compositor/desktop/src/input.rs`,
in the desktop track `issues/toyos-has-a-desktop.md`.

## Exit condition

A host test of `fold_mouse` (or what replaces it) in which one read carrying
two clicks at two positions yields both, each at its own position.
