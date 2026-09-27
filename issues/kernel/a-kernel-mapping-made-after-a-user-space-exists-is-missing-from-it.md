---
status: open
kind: defect
opened: 2026-09-27
---

# A kernel mapping made after a user space exists is missing from that space

`AddressSpace::new_user` (`kernel/src/arch/x86_64/paging.rs`) copies the kernel
root's present entries 256..512 once, at creation. `paging::init` creates only
the entries the direct map reaches (`toyos_memmap::direct_map_end`: the low
4 GiB and the memory above it), and `map_mmio` creates any other through
`ensure_table`. An entry created after a user space was made is absent from
that space, so a kernel access through it under that space's CR3 is a
not-present fault in Ring 0.

`pcidev::place_bar` reaches it: a claim is a syscall on the claimant's CR3,
and `probe_dword` maps and reads the BAR where firmware put it. Firmware puts a
64-bit BAR in its 64-bit window. Nothing orders that window below 512 GiB:
under Homebrew QEMU 11.1.1's edk2 with `-cpu qemu64` the window is
`0xc000100000+0x1ffff00000` (the loader's `GCD:` line), which is root entry
257. The repository's `ovmf/` puts the kernel's own xHCI BAR in its 64-bit
window too (`xHCI: BAR0=0x800004000`); if edk2 does the same, the kernel maps
entry 257 at boot, before any user space, and hides this. Unmeasured: no boot
here has printed edk2's BARs. A machine with no kernel-driven function in that
entry faults on netd's claim. `probe_dword`'s comment says "the boot
map already covers every physical address", which is false.

**Exit condition**: every kernel root entry a physical address this CPU can
name reaches through the direct map exists before the first user space copies
them, and a later install of one is a named panic.
