---
status: assigned
kind: defect
opened: 2026-10-13
---

# An aarch64 `cargo run` boot hands over no PCI claim

`cargo run -- --arch aarch64` launches `virt` with the GICv3's ITS and
the SMMUv3, with the console's virtio-serial function behind the unit
(`build/qemu.rs`). It reaches no userland: the kernel panics in
`kernel/src/drivers/virtio.rs`'s `wait_used`, its console's first queue
having completed nothing in five seconds.

The kernel's virtio console comes up before the `acpi` claim's holder has
declared the root bridges' windows, and every domain is built clear of
them (`kernel::iova`), so the console is given no domain of its own
(`iommu: no domain of its own for a device: no declaration of the root
bridges' windows is held`) and keeps the machine's addresses. On VT-d that
is the identity domain: the console works, but loses the domain of its own
`main` gives it, built clear of the loader's window list. On the SMMUv3 a stream with no domain aborts what it is
given, so the console's DMA reaches nothing.
Measured at #935's merge of `main` (`3962d606f`), QEMU 11.1.1 under HVF.

Before that merge a domain was placed above the CPU's addresses where the
unit translates wider, which needed no declaration. `main`'s IOVA
allocator removed that rule, and `main` still builds its domains clear of
the loader's window list, which #935 removes. #935's own boot at
`4f5142fea`, before the merge, handed over all four claims (diskserver's
NVMe, soundserver, netstack, vdagent).

Owner: the PCI apertures track (#935). How a driver in this kernel gets a
domain before any declaration is the owner's decision. The arm64 track's
stage 7 exit (`issues/toyos-runs-on-arm64.md`) waits on it.

**Exit condition.** A `cargo run -- --arch aarch64` boot hands over a
claim: its console shows the disk driver serving its NVMe partitions and
`vdagent: port 1 open`.
