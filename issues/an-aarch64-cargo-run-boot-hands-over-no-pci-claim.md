---
status: assigned
kind: defect
opened: 2026-10-13
---

# An aarch64 `cargo run` boot hands over no PCI claim

`cargo run -- --arch aarch64` launches `virt` with the GICv3's ITS, on
QEMU's own GIC under HVF (`Arch::irqchip`), which arms it and maps each
claim's DeviceID to its slot's LPI: the same launch by hand with
`its=on,kernel-irqchip=off` (QEMU 11.1.1) showed it. It still hands over
none: diskserver's NVMe, soundserver, netstack and the clipboard's
virtio-serial function (vdagent's when that boot ran) are each
refused with `its BAR was moved onto 8 64-bit address(es) inside the
windows firmware declared and the function answered at none of them` (lines 343, 418, 450, 487 of
[the boot's log](https://github.com/ToyOSOrg/evidence/blob/e626cced7ab73c04efdbbfd0b98a2f0a0ff8d86b/fix2-60f08b937/aarch64-its-boot.log)).
Each answers at its firmware BAR: `0x0f0107ff`, `0x79000000`,
`0x30bf8024` and `0x00000103` (lines 327, 386, 433, 470). At every
address it is moved onto the read is `0xffffffff`, except the first,
`0x4010000000`, the ECAM base (line 212), which line 225 lists among the
windows firmware declared: there the NVMe's read is `0x00081b36`
(line 327) and the other three's `0x00000000`.
The kernel's own xHCI driver is given no LPI there (`not armed — a driver
in this kernel is given no LPI`) and initialises no controller, so no USB
keyboard reaches the compositor, which panics on "the manifest gives this
program the keyboard".

So on aarch64 the virtio-serial driver's claims and handshakes are unbooted:
it is handed neither the console's function nor the clipboard's, so the
console port `cargo run`'s terminal shows carries nothing, and the boot's
whole log is on the UART's file (`/tmp/toyos-uart-early.log`). Its
handshakes on aarch64 rest on the code that passes on q35.

The windows a claim's BAR is checked against come from the loader's list, which takes every
memory range firmware's map types MMIO and nobody owns, ECAM and flash among
them; the root's own `_CRS` on this machine declares `0x10000000..0x3eff0000`
and `0x8000000000..0x10000000000` and neither. Taken up by #916: its stage A
holds the declaration as a kernel state, and its stage B makes acpiserver
declare each root's `_CRS` on both architectures, which is what hands these
four claims over and closes this file.

Owner: the arm64 track, `issues/toyos-runs-on-arm64.md`, whose stage 7
exit (`ssh` from the host, the desktop on virtio-gpu) a `cargo run` boot
cannot meet until this is fixed.

**Exit condition.** A `cargo run -- --arch aarch64` boot hands over a
claim: its console shows diskserver serving its NVMe partitions and
`vdagent: the host announces capabilities`.
