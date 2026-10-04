---
status: open
kind: defect
opened: 2026-10-04
---

# A memory-overwrite request Ubuntu left set stays set under ToyOS

The owner's ruling "Keep crash records" (2026-10-03,
`issues/diagnostics/toyos-explains-itself.md`) has ToyOS not ask the firmware
to wipe memory at reset. On the T14 the request is already made: under its
Ubuntu (`6.8.0-142-generic`), whose EFI stub sets it on every boot
(`issues/boot-media/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`),

```
ls /sys/firmware/efi/efivars/ | grep -i -E "MemoryOverwrite|MOR"
for f in /sys/firmware/efi/efivars/MemoryOverwriteRequestControl*; do [ -e "$f" ] && { echo "$f"; od -An -tx1 "$f"; }; done
```

read `07 00 00 00 01` for `MemoryOverwriteRequestControl` and `07 00 00 00 00`
for `MemoryOverwriteRequestControlLock`: efivarfs's four attribute bytes
(non-volatile, boot-service and runtime access), then the value: the request
0x01, set, and its lock 0x00, unlocked. The variable is
non-volatile, so it stays set into every ToyOS boot that follows Ubuntu, and
`bootloader/src` neither reads nor writes it. Whether this firmware honours it
on the reset paths ToyOS takes is unmeasured: a black-box record has survived
two hours of Ubuntu, which does not say what an unclean reset does. Every
UEFI call ToyOS makes is the loader's, so the write belongs with
`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`.

**Exit**: where the firmware defines `MemoryOverwriteRequestControl` and it
reads non-zero, the loader writes 0 before `ExitBootServices` and logs the
value it found; where the firmware defines none, it makes no `SetVariable`
call under that GUID. A guest test on a fresh OVMF variable store planted with
1 reads 0 after the loader's pass, and one planted with nothing finds no
variable under the GUID; deleting the write reds the first, and an
unconditional write reds the second. On the T14, a boot that finds it set
logs the 1 it found, and a black-box record sealed by a forced reset of that
boot is read by the next.
