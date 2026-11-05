---
status: open
kind: defect
opened: 2026-11-05
---

# A large spawn's image read costs about 85 ns a page more than main's

`osbench`'s `proc.spawn_wait.large` spawns a 32 MiB program from a served
file. The spawner reads the image through `SYS_PAGED_READ` into fresh shared
memory (`toyos::process::Image::read_for`). #1150 copies each of those pages
through the reader's own mapping (`arch::uaccess`), after faulting each chunk
of 16 frames in first (`PagerObject::read`, `UserBytesMut::fault_in`). Main
copied them through the kernel's direct map.

Measured with every page faulted in before its copy:
- KVM on the guest runner: 8.37-8.42 ms against main's 7.63-7.79 ms. That is
  about 85 ns per 4 KiB page written (evidence `ToyOSOrg/evidence`, branch
  `pr-1150`, r11).
- The T14, metal: 9.02-9.19 ms against 8.32-8.32 ms, +9.3% (same branch, r12).

**Hypothesis, unmeasured:** the cost is translating each destination page
through the process's 4 KiB mapping. The kernel's direct map uses huge pages,
so main's copy took no TLB fill for a fresh page, and a nested page walk under
KVM costs more than a native one. No counter has been read to confirm it.

**Exit:** on the T14 the large spawn is within main's run-to-run range, or the
per-page difference is measured (TLB misses and walk cycles by performance
counter, against main) and shown to be the cost of copying through the user
mapping that #1150's design accepts, with that figure recorded here.
