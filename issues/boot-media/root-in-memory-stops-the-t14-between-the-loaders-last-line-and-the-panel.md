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

The next run is the instrument: the loader and the kernel now paint the
handoff squares (`toyos-bootmap/src/mark.rs`), so the photograph says which of
`ExitBootServices`, the `mov cr3` and the kernel's entry the machine reached.

Exit: the stop located by those squares, its cause fixed where it lives, and a
T14 run of the branch reaching `Boot: complete`.
