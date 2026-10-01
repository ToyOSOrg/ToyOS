---
status: open
kind: defect
opened: 2026-10-01
---

# A TCO base word near all-ones panics the loader and the kernel

`toyos_tco::Chipset::port` (`toyos-tco/src/lib.rs:275-289`) refuses an
all-ones base word by name, but under Tiger Lake-LP's row (`8086:a0a3`,
`base_mask: !1`) a word from `0xFFFF_FFEE` to `0xFFFF_FFFE` reaches `:284`,
where `base + base_offset + TCO_TMR + 1` overflows `u32`. The loader and the
kernel each read the word out of the PCH's configuration space and hand it to
`port` (`bootloader/src/watchdog.rs:141`, `:73`;
`kernel/src/arch/x86_64/watchdog.rs:51`, `:58`), and both build with
`overflow-checks`. So a boot that arms the watchdog on a machine whose function
answers one of those words panics where it should refuse.

A host program calling `port` with every `u32` base word and the row's enable
bit, built with `overflow-checks`, panics with "attempt to add with overflow"
at `toyos-tco/src/lib.rs:284:19` for 17 words under `8086:a0a3`,
`0xFFFF_FFEE` the first and `0xFFFF_FFFE` the last, and for none under q35's
`8086:2918`.

**Exit:** `port` refuses, by name, every base word whose block does not fit
the I/O space, and a host test in `toyos-tco` asks it `0xFFFF_FFFE` under the
`a0a3` row.
