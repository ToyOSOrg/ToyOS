---
status: open
kind: defect
opened: 2026-09-29
---

# An AArch64 crash report reads through any user leaf

`kernel/src/arch/aarch64/paging.rs`'s `read_user_word`, which the report
of an EL0 fault calls with the faulting thread's own `x29` to walk its
frames (`trap.rs`'s `user_backtrace`), reads the frame any valid user leaf
names through the direct map. The direct map holds memory and nothing
else, so a leaf naming a device's registers — a claimed function's BAR,
once the port's stage 6 maps one into a process — names an address the
direct map does not hold, and the report's read of it is an EL1 data abort:
a user fault with `x29` pointed into its own BAR ends the machine.

Two more readers take the same leaf to the direct map:

- **Every syscall's user copy.** `AddressSpace::leaf` answers a Device
  leaf as it answers a Normal one, for a `Write` too where the leaf is
  EL0 read-write, and `kernel/src/user_ptr.rs` copies through
  `DirectMap::from_phys` of what it answered: `translate_now`, `object_run`
  (`copy_in`, `copy_out`) and `View::pieces` (`UserBytes::read_at`,
  `UserBytesMut::write_at` and their kin). A `read(fd, bar, n)` is an EL1
  abort on an address the direct map does not hold, or, where `map_mmio`
  holds it Device-nGnRE, an alignment fault on `memcpy`'s unaligned access:
  a kernel panic a process chose.
- **The fault dump.** `kernel/src/process.rs`'s `dump_crash_diagnostics`,
  which AArch64's `trap.rs` calls on an EL0 fault, reads the words around
  the fault address and the faulting PC through `translate` and the direct
  map, the same way.

Nothing maps a BAR into an AArch64 process yet — `arch::msi_message`
refuses there, so pcidev refuses every hand-over — so this is latent until
stage 6 of `issues/kernel/toyos-runs-on-arm64.md`.

**Exit condition**: `read_user_word`, `leaf` and the fault dump's
`translate` answer only a leaf of the memory type the direct map holds, and
guest tests see a process end and the kernel live when it faults with `x29`
inside a mapped BAR, faults at an address inside one, and names one as a
`read`'s buffer — the last refused with `BadAddress`.
