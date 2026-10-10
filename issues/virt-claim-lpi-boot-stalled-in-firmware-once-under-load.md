---
status: open
kind: defect
opened: 2026-10-10
---

# `virt_claim_lpi`'s boot stalled in firmware once under load

One full `cargo test` on the dev host, run beside a `cargo run -- --ci host`
at a load average of about 55 to 60 on 14 cores, red `virt_claim_lpi` with
`Boot timed out waiting for control registers: SCTLR_EL1=`. It was the
suite's last guest and booted alone. Its console stopped after the firmware's
`UsbBootExecCmd: Success to Exec 0x0 Cmd (Result = 1)`, the line a green boot
follows with `BdsDxe: loading Boot0001 "UEFI QEMU QEMU USB HARDDRIVE
TOYOS0BOOTSTICK1"`, and said nothing more in the 63 s `BOOT_CEILING`
(`tests/common/qemu.rs`) gives it. Its machine is `Profile::VirtIts`, the one
HVF profile on QEMU's own GICv3 (`its=on,kernel-irqchip=off`).

The ceiling is not the cause. The same machine, booted from its image and
waited on `SCTLR_EL1=`, reached it in at most 7 s in each of about 4,000
boots, about 2,200 of them on firmware variables fresh from the template, 6
to 12 at a time, at loads up to 130, beside
a guest suite and `--ci host`, under memory pressure and under sustained disk
writes. None stalled, so what stopped the firmware is not known.

`b65ba04fe` deleted `virt_claim_lpi` for it, with everything only it
reached: `Profile::VirtIts`, `tests/virtclaimcase` and the `claim_lpi`,
`claim_unrouted` and `claim_bar_fault` suite binaries. The test is restored
as it stands at `ec8a746b1`, the commit that also fixed its other red, the
kernel's lines of a job judged before they reached the PL011: `git revert
b65ba04fe`. Until then nothing reaches the ITS path the arm64 track's stage 6
names (`issues/toyos-runs-on-arm64.md`).

Owner: the arm64 track's stage 6, whose ITS path has no test until this
closes.

## Exit condition

The stall is reproduced by a deliberate harness, not by the suite: parallel
boots of the restored `Profile::VirtIts` machine, each waited on
`SCTLR_EL1=` and run with `-action reboot=shutdown -action shutdown=pause`,
the registers of one that stalls read over QMP before QEMU is killed
(`tests/CLAUDE.md`, a death during boot is reproduced by boots). The cause is
named from them and fixed where it lives, and `virt_claim_lpi` is restored by
reverting `b65ba04fe` in the same change.
