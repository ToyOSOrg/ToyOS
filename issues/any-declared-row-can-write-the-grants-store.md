---
status: open
kind: defect
opened: 2026-10-10
---

# Any declared row can write the grants store

`/system/bin/supervisor` keeps every answer about a folder of the home in
`/state/supervisor/grants` (`toyos_manifest::grants::STORE`): which folder a
package's exact binary is granted, and which binaries were denied. The
supervisor is the only program meant to write it, and it reads it back at boot
refusing by name anything its own renderer cannot have written.

But every row the image declares sees the whole tree read-write
(`toyos_manifest::whole_tree`), `/state` included. So any declared program, a
shell and everything a shell starts among them, can write a line granting a
package any folder the store's parser accepts, or delete a Deny, and the next
boot honours it without asking the person at the screen. Such a row already
reaches the whole tree itself, so it gains no file it lacks; what it gains is
a package's access, without the consent the prompt exists to ask for.

The store is read once at boot, so a rewrite made while the machine runs takes
effect at the next boot.

## Owner

`issues/every-program-sees-only-the-files-it-was-given.md`, stage 2: every
row declares its files, and nothing still sees the global tree.

## Exit condition

No row but the supervisor's own view holds `/state/supervisor` writable, and a
guest test whose job writes a grant line into the store from a shell is
refused by the file server.
