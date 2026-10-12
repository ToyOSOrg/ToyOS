---
status: open
kind: defect
opened: 2026-10-12
---

# The AP trampoline is written at 0x8000 whatever the memory map says is there

`kernel/src/arch/x86_64/smp.rs` copies the application processors' start-up
code to physical page `0x8000` (`TRAMPOLINE_PAGE`) and starts every AP there.
`arch::boot::reserved` withholds the page from the allocator, and
`kernel_main` (`kernel/src/main.rs`) checks every region it withholds against
the firmware's map except this one: "The architecture's own page is not a
loader allocation, so it is named here". Nothing asks whether the page is
memory at all, nor whose.

It holds on QEMU's q35, whose edk2 hands over `0x0..0x87000` as conventional
memory (the map recorded in `lib/bootmap/tests/direct_map.rs`), and the T14
starts its CPUs through it; how the T14's map types the page was not read. It
is a fixed address chosen for one layout: a firmware that keeps anything of
its own at `0x8000` — runtime code, ACPI NVS, a reserved range — has it
overwritten when the first AP starts, though the map the kernel was handed
would have said so. No other hypervisor's map was read for it.

What the architecture needs is a page below 1 MiB, its number the `STARTUP`
IPI's vector (Intel SDM Vol. 3A, "MP Initialization Protocol Algorithm"), and
UEFI lets the loader ask for exactly that before `ExitBootServices`:
`AllocatePages` with `AllocateMaxAddress` (UEFI 2.10 §7.2), the page then
`LoaderData` like every other region `kernel_main` checks.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: the trampoline's page is one the loader allocated below 1 MiB and
hands over in `KernelArgs`, held by the check `kernel_main` makes of every
other withheld region; deleting the allocation reds that check at boot.
