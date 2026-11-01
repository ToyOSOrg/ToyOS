---
status: assigned
kind: defect
opened: 2026-10-13
---

# An aarch64 `cargo run` boot shows nothing on its terminal

`cargo run -- --arch aarch64` launches `virt` with the GICv3's ITS and
the SMMUv3, with the console's virtio-serial function behind the unit
(`build/qemu.rs`). Its terminal shows nothing after the loader: the whole
log is in `/tmp/toyos-uart-early.log`, the PL011's file.

The kernel's virtio console comes up before the `acpi` claim's holder has
declared the root bridges' windows, and every domain is built clear of
them (`kernel::iova`), so a driver in this kernel is given no domain of its
own and keeps the machine's addresses (`iommu::DeviceSpace`). On VT-d that
is the identity domain, and the console works. On the SMMUv3 a stream the
IORT routes through the unit and no claim gave a domain aborts what it is
given, so the console refuses the function
(`virtio-console: … would reach no memory — device refused, and the
console stays on the UART`) rather than wait on a queue that completes
nothing, which panicked the kernel in `wait_used` before #934 (measured at
#935's merge of `main` `3962d606f`, QEMU 11.1.1 under HVF).

Its claims are handed over since the `acpi` claim's holder declares the
root bridges: the NVMe driver serves its partition and `vdagent: port 1
open` is in the file. The refused console function is left to a claim, and
vdagent's row matches it: the second vdagent panics on it ("it negotiated
no VIRTIO_CONSOLE_F_MULTIPORT").
Read at #934's `f47e0af18`.

The owner's ruling: the virtio console moves to a userland driver over an
ordinary claim, with the UART for early boot and the fatal path, and a
driver in this kernel gets no IOMMU exception. That is #1062, which deletes
`DeviceSpace` with the console.

Owner: the arm64 track, `issues/toyos-runs-on-arm64.md`, whose stage 7 exit
waits on it.

**Exit condition.** A `cargo run -- --arch aarch64` boot shows its log on
its terminal, the disk driver serving its NVMe partitions and
`vdagent: port 1 open` among it.
