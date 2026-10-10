---
status: owner
kind: question
opened: 2026-10-03
---

# ToyOS's own records carry the stick's serial, the card's MAC and the resolvers in full

Nothing that identifies the owner's machines or network goes into the tree, a
commit message or anything posted on GitHub (root `CLAUDE.md`). ToyOS prints
three such values in full on every boot of the T14, so every T14 log pasted in
public carries them unless whoever pastes it masks them by hand:

- the USB stick's serial number: `kernel/src/drivers/xhci/wait/msc.rs:1407`,
  on every bind, and `:1415`, when a disk comes back;
- the network card's MAC: `userland/netstack/src/main.rs`, `main`;
- the resolvers the lease named, which on the bench are the provider's public
  ones: `userland/netstack/src/main.rs`, `Leases::pass`.

`src/sourcegate.rs` reads tracked files only. A pull request's body, a comment
and a commit message are read by nothing before they are public.

## The question

Should ToyOS print these values at all, or do they stay in its records and
whoever pastes a log masks them?

## Exit condition

The owner's answer. If the system is not to print them, the three records
change. If the paster masks, this file is deleted.
