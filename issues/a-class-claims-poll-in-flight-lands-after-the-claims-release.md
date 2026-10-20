---
status: open
kind: defect
opened: 2026-10-08
---

# A class claim's poll in flight lands after the claim's release

A claim on a class this kernel still drives — the keyboard and the mouse — is
a per-class flag (`device::TAKEN`). The watch a poll on one registers on is the
class's one `static` (`keyboard::WATCH`, `mouse::WATCH`, by `ops::read_watch`),
and `inbox::arm` registers on it with nothing held. A sibling thread's close
between the resolve and the registration lets the poll land after the close
answered the watch's polls (`ops::close`), and the class's next holder's first
event fires it into the old process's ring: one bit of another process's
device activity. A claim on a PCI function, an ISA function or the ACPI fixed
hardware registers with what the claim lends (`DeviceClaim::add_poll`) and
does not have it.

**Read from the code, not run.** A guest cannot order a close and a second
claim between the two steps of one syscall.

**Exit condition**: no poll registered on a released class claim stays on its
watch once the class's next claim is minted; or the class is gone from the
kernel, as `issues/every-driver-is-still-in-the-kernel.md` retires each of
them.

**Owner**: none. That track is open and nobody holds it.
