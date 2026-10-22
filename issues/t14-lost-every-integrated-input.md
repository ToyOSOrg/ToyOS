---
status: open
kind: defect
opened: 2026-08-03
---

# The T14 lost every integrated input at 6.6 s, and the log cannot yet say why

All three integrated pointers and the keyboard are behind the one i8042, and all
three went dead 6.6 s into the 2026-08-03 compositor session. The whole of what
the driver said about it, and the last `i8042:` line in a 58-second log:

```
[kernel 6.594 cpu0] i8042: 1 interrupts and 1 bytes, nothing decoded — no event from [aux 0x08], first seen at 6594ms
[kernel 6.609 cpu1] i8042: the pin asserts — 6 interrupts, 6 bytes, 0 keys, 2 motion, no event from [aux 0x08, aux 0x06, aux 0x08, aux 0x0e], first seen at 6594ms
```

**That line does not say what it looks like it says, and the first task on it was
opened on the strength of the misreading.** `0x06` has bit 3 clear and no packet
head ever does, so the four listed bytes read as a framer that had lost the
frame. They are not. Six bytes, two motion events, four bytes named: 2 × 3 = 6,
and the four are the head and first body byte of two whole, correctly framed
packets — `0x08` is a resting head and `0x06` is a `dx` of +6. **The pointer was
framing perfectly.** The arithmetic is forced and no reader would do it.

Closed, therefore: the decoder did not desync, and no fix for a desync was
needed. What was wrong is the instrument, and it is fixed (`647c3c0`) —
`MouseOutcome` could not distinguish a byte held inside a packet from a byte
thrown away at a boundary, so two of every three bytes of a healthy pointer
stream were reported as suspects. `toyos-ps2`'s
`a_healthy_stream_discards_nothing_and_leaves_no_byte_unaccounted`
(`system/ps2server/ps2/tests/decode.rs`) holds that with the T14's two packets.

The kernel driver that logged this session is gone: `/system/bin/ps2server`
drives the i8042 from userland (#899) and resets both devices itself
(`system/ps2server/src/probe.rs`). Nothing since has read a T14 session long
enough to see whether the loss recurs under it.

**What remains open is the actual question, and the log cannot answer it.**
The tally counts in the ISR before any decoding, so 6 interrupts is hardware truth
— but it is truth *as of 6.609 s*, which is when the driver stopped speaking:
that driver's health line was said once. For the remaining 54 s the log cannot separate:

- **the pin stopped asserting** — a wedged controller, a lost edge, an EC that
  stopped scanning, an RTE that got masked; from
- **bytes kept arriving and decoded to nothing** — a wire-format or framing
  fault, in this driver.

Those are opposite defects in opposite subsystems and the counters that tell
them apart were read once. Two facts are established and neither settles it: all
six bytes were aux (four named `aux`, two produced motion, `0 keys`), and **the
keyboard produced no byte at all in 58 s** — not "stopped at 6.6 s", never. The
same machine's earlier boots drove a shell off that keyboard
(`c7efcd30d^:specs/reference/metal-hardware-inventory.md`), so it is not a routing fault.

ps2server's counters line is what makes the next session decisive rather
than a guess: it repeats at most once per 10 s and **only when an interrupt has
come since the last line** (`Stream::counters`, `system/ps2server/src/stream.rs`).
Past the first line, no line means no interrupt, so silence becomes evidence
instead of absence of evidence.

**What the next session should capture.** A `ps2server:` counters line dated
after the inputs go dead, or none. If bytes are arriving, `undecoded` and
`discarded` name the fault in the decoder. If no line appears at all, the pin is
not asserting and the next suspect is the controller or the EC — and nothing in
`toyos-ps2` can be responsible.

Two things deliberately not concluded:

- **The touchpad is not evidence of a mux problem.** The T14's touchpad is
  I2C-HID off an LPSS controller that is not on the PCI bus at all; the EC
  mirrors it onto the aux port beside the TrackPoint. The aux device answered
  `0xF2` with id `0x00` — a plain 3-byte mouse — so the driver's 3-byte frame is
  what the wire carries, and the 4-byte IntelliMouse mismatch usually blamed for
  a PS/2 desync is not available here.
- **The USB mouse plugged in at 30.4 s produced no motion either**, which is
  `issues/the-t14-mouse-may-be-another-defect.md` and not this one.

`i8042_mouse` is deleted; `issues/i8042-mouse-ends-four-packets-short-with-a-clean-exit.md` records the commit that restores it.
