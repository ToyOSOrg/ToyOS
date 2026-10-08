---
status: open
kind: defect
opened: 2026-10-08
---

# A PCI call in flight names its function by a slot the next claim reuses

Every syscall on a PCI claim — the BAR, the DMA grants and mappings, the
config read — resolves its handle to a `pcidev` slot number
(`pci_slot` and the register target, `kernel/src/syscall/device.rs`) and lets
go of the claim before it acts. What
it then acts on is whatever `BOUND[slot]` holds when `with_bound`
(`kernel/src/pcidev/mod.rs`) takes that slot's lock, which checks that the
slot is bound and not faulted, and not whose binding it is. The two steps are
under different locks with nothing held across them.

A slot is not a name for one claim: the last handle's close releases it, a
function that was reset leaves it `Free`, and `slot::reserve`
(`kernel/pci/src/slot.rs`) gives the first free slot to the next claim,
whichever function and process that is. A call that resolved its slot before
its own claim was released therefore runs against the next holder's function,
and answers its caller with that function's object. A claim is authority over
one function for one holder; here it reaches another's.

**Read from the code, not run.** A call made after the close is refused at
the handle, so only a call already past `pci_slot` is exposed, and a thread
in a syscall holding no lock can be preempted there. A guest cannot order a
release and a second claim inside that interval; the test owes the kernel a
hold at that point, which is an actuator this tree does not have.

**Exit condition**: a call on a claim acts on that claim's binding or is
refused — its identity is held from the handle to the act, or the binding is
checked against it — and a test that holds a call between the two steps,
across the claim's release and the slot's next claim, sees it refused with
the second holder's function untouched.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.
