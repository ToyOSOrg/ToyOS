---
status: open
kind: defect
opened: 2026-10-08
---

# A class claim's call in flight acts after the claim's release

A claim on a class this kernel still drives — the framebuffer, HDA audio — is
a per-class flag (`device::TAKEN`), and a call on one checks
the handle and then acts with nothing held across the two:

- `SYS_DEVICE_REG_READ` and `SYS_DEVICE_REG_WRITE` resolve the claim to
  `RegTarget::Hda` under the process-data lock (`sys_device_reg`,
  `kernel/src/syscall/device.rs`) and reach `drivers::hda::reg_write` after
  it;
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
lock its release takes it with (`DeviceClaim::pci`, `DeviceClaim::isa`). That
borrow is no fix to copy here as it stands: `SYS_GPU_SET_RESOLUTION` allocates,
and the claim's lock is a spinlock.

**A poll has the same shape on these classes.** The watch a poll on an audio
claim or the mouse's registers on is the class's one `static`
(`drivers::AUDIO_WATCH`, `mouse::WATCH`, by `ops::read_watch`), and
`inbox::arm` registers on it with nothing held. A sibling thread's close
between the resolve and the registration lets the poll land after the close
answered the watch's polls (`ops::close`, and for audio `Claim`'s drop in
`kernel/src/device.rs` too), and the class's next holder's first interrupt
fires it into the old process's ring: one bit of another process's device
activity. A claim on a PCI
function, an ISA function or the ACPI fixed hardware registers with what the
claim lends (`DeviceClaim::add_poll`) and does not have it.

**Read from the code, not run.** A guest cannot order a close and a second
claim between the two steps of one syscall.

**Exit condition**: no call on a released class claim reaches the device, and
no poll registered on one stays on its watch, once the class's next claim is
minted; or the class is gone from the kernel, as
`issues/every-driver-is-still-in-the-kernel.md` retires each of them.

**Owner**: none. That track is open and nobody holds it.
