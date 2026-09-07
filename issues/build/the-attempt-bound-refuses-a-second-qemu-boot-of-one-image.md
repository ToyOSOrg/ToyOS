---
status: open
kind: defect
opened: 2026-09-07
---

# The attempt bound refuses the second QEMU boot of one image, and `boot_partition_identity` is red on `metal-suite`

`bc4bb97e` added the loader's bound on a hang: an image whose last boot was
handed the machine and reported nothing costs that image one boot, so a
power-cut owner gets a pass that boots no kernel instead of the same hang for
ever (`bootloader/src/attempt.rs`, `is_the_retry`).

`is_the_retry(has_a_page, harvested, previous)` is `has_a_page && !harvested &&
previous >= 1`. **A healthy QEMU boot harvests nothing** — QEMU zeroes a
machine's RAM between launches, so the page is always empty — and the count
lives in the image file, which the harness reuses across launches. So the second
launch of one image is indistinguishable from a retry after a hang:

```
$ cargo test boot_partition_identity
[serial 3] Boot attempts: this image has had the machine 1 time(s) without reporting; now 0
[serial 3] Boot attempts: the previous boot of this image never reported; the machine is handed back
  FAIL  boot_partition_identity  (3s)
```

`boot_partition_identity` boots one image more than once, and every boot after
the first now boots no kernel: `[qemu] QEMU died before ===READY===`.

**Measured against `origin/metal-suite` alone**, checked out and run in the same
session: red there too, twice. It is not the boot-deadline branch's, which is
only recording it here because its own fold ran the gate that found it.

`hang_bounded_by_the_stick` passes because it launches an image whose boots
each end in a reset the loader accounts for; the ordinary suite does not.

What distinguishes the two cases is what the *guest* did, not what the page
holds — a boot that reached `Rebooting.` reported, whether or not its record
survived a launch boundary — so the bound needs a signal that survives a QEMU
relaunch the way it survives a power cut, or it has to be off where the harness
relaunches one image.

**Exit condition**: `cargo test boot_partition_identity` green with the bound
still refusing the second boot of a genuinely hung image, which
`hang_bounded_by_the_stick` is the assertion for.
