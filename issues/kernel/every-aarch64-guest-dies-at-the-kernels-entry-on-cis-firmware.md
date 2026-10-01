---
status: open
kind: defect
opened: 2026-10-01
---

# Every AArch64 guest dies at the kernel's entry on CI's firmware

All sixteen `virt_*` tests red in CI. Each boot's console ends the same way:
`Loader log: the kernel handoff begins`, then the firmware's
`Synchronous Exception at 0x00000000BC33EB20`. The PC differs per kernel build,
but always falls inside the kernel image the loader placed at `0xbc200000`. That
happens at EL1 entry (`Profile::Virt`) and at EL2 entry (`VirtEl2`), on one CPU
and on eight. The kernel prints nothing first, so the tests time out waiting for
their first marker.

**Evidence**, identical in two runs on toolchain
`toolchain-linux-x86_64-48dd24f826263d6c`:
- Main's nightly `guest` lane at `06788146b`, run 36843762360, job 110374194368.
- PR #671's `guest` check, on its merge onto `59052827f`, run 36863809437, job
  110375742604.

Both ran QEMU 11.1.1 under TCG `-cpu max` on an AMD EPYC 9V45 with 4 cores. The
firmware was Debian's `AAVMF_CODE.no-secboot.fd`, "version 2026.05-2". Both
logged `test result: FAILED. 4 passed, 17 failed`. The other red is
`issues/kernel/the-nested-nmi-report-interleaves-with-another-cpus-console-line.md`.

The same sixteen tests pass on the dev host, in a whole-suite run of #670's
branch (`test result: ok. 21 passed`). That host ran QEMU 11.1.1 from
Homebrew, under the same TCG `-cpu max` for `VirtEl2`. Two parts of the
instrument differ:
- The firmware: the dev host runs QEMU's bundled `edk2-stable202408-prebuilt.qemu.org`.
- The toolchain: the dev host builds its own, and CI restores the sysroot its
  toolchain job built.

`.github/qemu-version` pins neither of the two.

**Exit:** the sixteen `virt_*` tests are green in CI's `guest` check.
