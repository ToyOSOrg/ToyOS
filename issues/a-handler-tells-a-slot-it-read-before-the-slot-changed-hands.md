---
status: open
kind: finding
opened: 2026-10-08
---

# A handler tells a slot it read before the slot changed hands

Two interrupt handlers name a `pcidev` slot by a number and write that slot's
record a moment later, with nothing excluding a release in between:

- the remapping fault handler reads the faulting stream's slot
  (`owner_of`, `kernel/src/arch/x86_64/vtd/fault.rs`) and then calls
  `pcidev::note_fault(slot)`;
- a claimed function's vector handler is its slot (`idt/user_dev.rs`) and
  calls `pcidev::isr(slot)`.

`pcidev::tear_down` clears the stream's slot and takes the remapping entry
away before the slot is free, so a handler that *starts* after a release names
nothing. One already past its read on another CPU still holds the number: if
the slot is released and bound to a second function before its write, that
function's claim is told a fault — every call on it refuses `Io` from then
on — or counts one interrupt its device did not raise.

The interval is two adjacent statements of a handler running with interrupts
off, and a second function's bring-up in between is config-space writes and,
after a reset, a wait of at least the reset's settle time; no authority moves
either way. Nothing in the tree can hold a handler there, so this is read and
not run.

**Exit condition**: the slot's record says which binding it is for and a
handler's write is refused by a record that has moved on, or this is folded
into `kernel/src/pcidev/mod.rs`'s header as the bound it is.

**Owner**: none. No track covers a slot's record.
