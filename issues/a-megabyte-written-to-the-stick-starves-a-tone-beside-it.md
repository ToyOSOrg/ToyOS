---
status: open
kind: defect
opened: 2026-09-26
---

# A megabyte written to the boot stick starves a tone playing beside it

`audio_tone_load` at eight CPUs drops out when a program writes a megabyte to
`/log` while the tone plays. Seen on `wt/toyos-logtrack`, whose `logd` then
created each part at its whole length (one mebibyte of zeros, written back
while the test's tone played): four runs, 181, 183, 185 and 189 underruns of
32 allowed, at host load averages from 19 to 35; the same tree with the part
created empty, 0 of 32 at load 25. The failing windows are the client
starving — soundd's `deferred` in the thousands and `starve_max` a whole
window — beside 15 564 xHCI interrupts on cpu0, the CPU that also takes the
sound device's, and a `usb-storage … transport broke on SCSI 0x2a: no answer
in the status phase in 2000 ms` in the middle of the tone.

Not investigated: which of the interrupt load on cpu0, the two-second transport
break, or the write-back holding what the client waits on is what starves it;
and whether real hardware shows it, where the stick is not an emulated one.
Nothing in `logd` is special about the write: any program writing that much to
the stick while a tone plays is the same stimulus.

## Exit condition

On the T14, a `METAL` row plays `hda_tone`'s tone while a program writes a
mebibyte to `/log`, and soundd reports `underruns=0` over the tone's window
across repeated boots. Owner: the metal suite (`tests/toyos.rs`'s `METAL`).
