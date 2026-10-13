---
status: open
kind: defect
opened: 2026-10-13
---

# An aarch64 `cargo run` boot hands over no PCI claim

`cargo run -- --arch aarch64` launches `virt,gic-version=3,iommu=smmuv3`
under HVF (`build/qemu.rs`, `machine`). HVF's own GIC has no ITS, so the
MADT names none (`ITS: the MADT names none`), and every claim is refused:
`pcidev: PCI … NOT HANDED OVER — its interrupts would not be remapped on
this machine`, for diskserver's NVMe, soundserver, netstack and vdagent.

The same launch with only the machine changed to
`virt,gic-version=3,its=on,iommu=smmuv3,kernel-irqchip=off` (QEMU 11.1.1,
by hand) arms the ITS and maps each claim's DeviceID to its slot's LPI,
and still hands over none: each of the four is refused with `its BAR was
moved onto 8 64-bit address(es) inside the windows firmware declared and
the function answered at none of them`, its config space answering
`0x0f0107ff` and the moved BAR `0xffffffff` (or another function's ID).
The kernel's own xHCI driver is given no LPI there (`not armed — a driver
in this kernel is given no LPI`) and initialises no controller, so no USB
keyboard reaches the compositor, which panics on "the manifest gives this
program the keyboard".

So on aarch64 the clipboard agent's claim and its handshake are unbooted:
its function is declined by the console driver and the console comes up
on the next one, and the agent itself exits on "no virtio-serial function
was handed to this program". Its handshake on aarch64 rests on the code
that passes on q35.

Owner: the arm64 track, `issues/toyos-runs-on-arm64.md`, whose stage 7
exit (`ssh` from the host, the desktop on virtio-gpu) a `cargo run` boot
cannot meet until this is fixed.

**Exit condition.** A `cargo run -- --arch aarch64` boot hands over a
claim: its console shows diskserver serving its NVMe partitions and
`vdagent: port 1 open`.
