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
- the network card's MAC: `userland/netstack/src/main.rs:1605`, and
  `src/metal.rs:2281`, where the host loop writes the one it read before the
  flash into the readback's `boot.txt`;
- the resolvers the lease named, which on the bench are the provider's public
  ones: `userland/netstack/src/dhcp.rs:149`.

`src/sourcegate.rs` reads tracked files only. A pull request's body, a comment
and a commit message are read by nothing before they are public.

The harness's judge prints neither: `tests/common/lan.rs:99` holds the MAC
record against the readback's without quoting either, and `:116` counts the
resolvers. The records it reads still carry all three.

## The question

Should ToyOS print these values at all, or do they stay in its records and
whoever pastes a log masks them?

## Exit condition

The owner's answer. If the system is not to print them, the three records and
the readback's key change, and the judges that read them with them. If the
paster masks, this file is deleted.
