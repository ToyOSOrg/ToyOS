---
status: open
kind: track
opened: 2026-09-07
---

# A driver is tested on the host, and its real implementation is one instruction deep

QEMU is the only thing that can run a driver today, so every driver test is a
boot, the suite's flakiness is the host's load, and a stub of the hardware's
own rules would have caught this month's hardware bugs before a machine did:
a wait that checked its deadline only on an empty ring, a port acknowledgement
that disables the port, a controller reset that is not what a device sees
(`issues/kernel/a-usb-wait-checks-its-deadline-only-on-an-empty-ring.md` and
the reset ruling in `kernel/src/drivers/acpi.rs`). Owner direction, 2026-09-07:
the logic inside every driver is tested on the host against stubs that
implement the hardware's behavior, errors and unpredictability; QEMU keeps a
few broad boots; the T14 keeps the numbers.

What is to be built:

- **The boundary, named by function and not by chip.** A driver is written
  against four small traits: register access, a clock, DMA buffers, and
  interrupt arrival. They are generic parameters resolved at compile time
  (the dispatch ruling in
  `issues/kernel/arm64-is-a-decision-nobody-has-made.md`: no trait objects,
  no `Kernel<A>`), and they are the same three things the userland-device
  substrate hands a process, so a driver moved to userland keeps its tests.
- **A real implementation is one instruction's worth of meaning per function**
  — a volatile access, a barrier, a register write, a counter read — with no
  branch, no policy and no state beyond an address. The architecture's memory
  ordering and cache rules live there and nowhere above it. Everything with a
  branch lives above the boundary, where the host tests it.
- **Stubs are written from the specification, cited by section, before the
  driver they test changes**, and they implement what the specification
  permits, not what ToyOS issues: out-of-order completion, a ring that never
  empties, a spurious event, a port that drops between two calls. Every stub
  takes a seed, every failure prints it, and the seed reproduces the run. A
  stub that returns what the code under test expects is a finding.
- **Recorded traces from the T14 are the oracle**: every register access and
  interrupt a driver sees on the laptop is recorded by the metal loop and
  replayed against the stub, which must reproduce the machine's answers.
- **Each driver moves with its tests.** Its logic tests move to the host and
  its QEMU registrations are deleted, never duplicated. Order: xHCI with USB
  storage and HID (where the bugs were, and the metal loop's stick traffic
  records it from day one), the IOMMU, NVMe, then HDA last and only with the
  owner's sign-off on audible output.

Blocked on: the metal suite landing and the userland-device substrate's first
pull request, since both are in the same drivers. This boundary is the
precondition of the ARM64 port: an architecture is one more implementation of
the same traits, and a trait with one implementation is a name, not a boundary.
