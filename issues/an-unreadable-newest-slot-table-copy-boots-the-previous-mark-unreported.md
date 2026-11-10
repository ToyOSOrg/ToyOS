---
status: open
kind: finding
opened: 2026-11-11
---

# An unreadable newest slot-table copy boots the previous mark, unreported

`toyos_update::slots::current` takes the other copy of the slot table when one
does not decode, and the loader (`Disk::slot_table`, `loader/src/rootimage.rs`)
says nothing of the copy it could not read. For a mark move that tore, that is
the designed atomicity: the machine boots the old mark or the new one. For a
newest copy that stops decoding after the mark moved (a block gone bad, a
stray write), the pass boots the slot the older copy marks and no line says
so. Found by inspection in the review of #1201; no boot has shown it.

Updates fix forward, and this is the one path left by which a pass boots the
previous slot. The table is unauthenticated and the slot it boots is still
held to the signature and the floor, so it changes nothing about what may run.

Owner: the update track (`issues/the-machine-updates-itself-without-ubuntu.md`).

**Exit**: the loader says which copy it read and why the other was not, in
`loader.log` and to the kernel, or the design rules that an unreadable copy
refuses the pass; a host test of `slots::current` holds whichever it is.
