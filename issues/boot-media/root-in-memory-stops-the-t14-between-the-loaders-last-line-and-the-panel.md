---
status: assigned
kind: defect
opened: 2026-09-25
---

# ROOT in memory stops the T14 between the loader's last line and the panel

Held by the branch of PR #506, which it blocks.

T14 runs 133 and 134, both on that branch: the loader reads ROOT whole
(`ROOT: read into memory at 0x5ed16000+0x2a00000 in 1683215644 TSC cycles` on
134), loads the kernel at `0x5e000000`, reports every handoff address inside
the boot map, and writes `Loader log: the kernel handoff begins, so this file
ends here`. Nothing is painted after it, not even `panic console: armed`, and
the machine sits until a hand resets it. Runs 125, 128 and 131, from branches
without ROOT in memory, boot the same machine; there the kernel sat at
`0x60a00000`.

What is ruled out, and how:
- the read size: 133 read ROOT in one request, 134 in 1 MiB chunks, and both stop the same way;
- the kernel's own code before `panic_console::arm`: `git diff origin/main`
  touches none of `_start`, `pat::init` or `arm`, so the kernel differs there only in the
  addresses it is handed and `KernelArgs`' length (`0x4c8` to `0x4e8`);
- QEMU/OVMF: the branch boots on 4 GiB and on 16 GiB with eight CPUs, the
  image off `usb-storage`.

What is not ruled out: the firmware's `ExitBootServices` after this loader's
raw whole-disk block reads and its `0x8000_7201` allocation, and a kernel
image or boot state the T14 corrupts at the new placement.

Run 137 carried the first instrument: the loader and the kernel paint the
handoff squares (`toyos-bootmap/src/mark.rs`), so the photograph says which of
`ExitBootServices`, the `mov cr3` and the kernel's entry the machine reached.

Run 137 (head 35826467) showed none of the three squares: the loader read ROOT
(`0x5ed16000+0x2a00000`), printed through the legend and `Loader log: the
kernel handoff begins`, and the row stayed dark. Read at face value that is
`ExitBootServices` not returning; the square had never been seen on this panel,
so the reading rests on an instrument with no positive control there.

The next run carries that control and one change: a fourth square, painted
leftmost just before the call through the same writes, and ROOT allocated as
`LoaderData` with `ROOT_IMAGE_MEMORY_TYPE` given only in the map the kernel is
handed (`toyos-bootmap/src/relabel.rs`). No square: the marks cannot be seen
here and 137 says nothing. One: the exit still does not return with no
OS-loader type in the firmware's map, so the type is not it and the 42 block
reads are next. Two or more: the type was it.

Exit: the stop located by those squares, its cause fixed where it lives, and a
T14 run of the branch reaching `Boot: complete`.
