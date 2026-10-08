---
status: open
kind: defect
opened: 2026-10-08
---

# A class claim's call in flight acts after the claim's release

A claim on a class this kernel still drives — the framebuffer, HDA audio,
virtio-sound — is a per-class flag (`device::TAKEN`), and a call on one checks
the handle and then acts with nothing held across the two:

- `SYS_DEVICE_REG_READ` and `SYS_DEVICE_REG_WRITE` resolve the claim to
  `RegTarget::Hda` or `RegTarget::VirtioSound` under the process-data lock
  (`sys_device_reg`, `kernel/src/syscall/device.rs`) and reach
  `drivers::hda::reg_write` or `drivers::virtio_sound::reg_write` after it;
- `SYS_GPU_PRESENT`, `SYS_GPU_SET_CURSOR`, `SYS_GPU_MOVE_CURSOR` and
  `SYS_GPU_SET_RESOLUTION` ask `holds_claim` and then call `crate::gpu`
  (`kernel/src/syscall/dispatch.rs`).

The flag clears when the claim's last handle goes (`Claim`'s drop,
`kernel/src/device.rs`) and the next claim sets it again. So a call whose
claim a sibling thread closed after the check, and whose class another process
claimed before the act, writes a register of the audio controller, or presents
on the display, that the second process now holds.

A claim on a PCI function, an ISA function or the ACPI fixed hardware does not
have this: its calls borrow what names the device from the claim under the
lock its release takes it with (`DeviceClaim::pci`, `DeviceClaim::isa`). The
same borrow closes these — the act made inside the claim's `Held`.

**Read from the code, not run.** A guest cannot order a close and a second
claim between the two steps of one syscall.

**Exit condition**: every call on a class claim acts under the lock its
claim's release takes, or is refused, and no syscall reaches a kernel-driven
device on a `holds_claim` answer alone.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.
