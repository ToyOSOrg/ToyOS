---
status: open
kind: defect
opened: 2026-10-10
---

# diskserver's NVMe bring-up open-codes what `toyos-pci-claim` holds

`Controller::open` (`userland/diskserver/src/nvme.rs`) maps BAR 0 with
`PciDev::map_bar` and an `unsafe Window::new` over it, allocates its grant with
`PciDev::dma_alloc` and another `unsafe Window::new`, and names each kernel
refusal as `Refusal::Kernel(call, e)`. That is `Bar::map`, `Grant::alloc` and
`KernelRefused` of `userland/toyos-pci-claim/src/lib.rs`, which netstack's two
drivers and soundserver's virtio-sound driver use: two more `unsafe` sites
whose argument is the one the crate already makes.

Exit: diskserver reaches its BAR and grant through `toyos-pci-claim`, or the
reason it cannot is at the crate's module header.
