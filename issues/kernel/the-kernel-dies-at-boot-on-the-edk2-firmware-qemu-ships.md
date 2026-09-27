---
status: expected-red
kind: defect
opened: 2026-09-27
---

# The kernel dies at boot on the edk2 firmware QEMU ships

Booted with `edk2-x86_64-code.fd` and `edk2-i386-vars.fd` from Homebrew's QEMU
11.1.1 (`/opt/homebrew/share/qemu/`), the kernel panics right after the
physical memory manager comes up:

    [kernel 0.000 cpu0 boot] pmm: the firmware map calls 2140844032 bytes usable in 110 entries; managed=2017460224 withheld=94371840 unaligned=29011968, and the three sum to it; frames=962 reserved_frames=45 base=0x200000 span=1022
    [kernel 0.000 cpu0 boot] EARLY PANIC: panicked at library/alloc/src/alloc.rs:659:9:
    memory allocation of 4096 bytes failed

The same image under the same command line with `ovmf/OVMF_CODE-pure-efi.fd`
and `ovmf/OVMF_VARS-pure-efi.fd` in their place reaches the desktop
(`compositor: ready`). Two things the firmwares hand over differ in the two
logs: the memory map, 110 entries against 95, and the GOP framebuffer, at
`0x80000000` against `0xc0000000`. On the firmware that boots, the line after
`pmm:` is the framebuffer's mapping, `mmio: 0xc0000000+0x400000 PAT
WriteCombining`.

The command line is `imagerelease::Host::MacosAppleSilicon`'s
(`src/imagerelease.rs`) with `-display none`, over a copy of a
`target/bootable.img`.

`release_command_boots` boots that line on the dev host, and is disabled in
`src/redlist.rs` while this stands.

**Exit condition.** `release_command_boots` is green on the dev host with the
firmware Homebrew's QEMU ships, and its row leaves `src/redlist.rs`.
