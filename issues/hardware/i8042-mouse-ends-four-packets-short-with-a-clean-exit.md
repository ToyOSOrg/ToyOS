---
status: expected-red
kind: tooling
opened: 2026-09-27
---

# `i8042_mouse` ends four packets short with a clean exit; mechanism not known

Seen on 2026-09-27 in the orchestrator's Fast-tier run on PR #537's head
`06b926b1`, a diff that touches neither the i8042 driver nor this test:

```
FAIL i8042_mouse: 872 pointer events reached userland out of 876 packets injected, never more than 4 of them (12 bytes) outstanding against a 16-byte device queue
  FAIL  i8042_mouse  (17s)
```

`cargo run -- --known-red i8042_mouse` answered NO. Earlier sightings of this
message are in `issues/build/parallel-tests-red-under-other-suites.md`'s
`i8042_mouse` entry.

## What is known

- **The run ended cleanly.** This message is reached only when
  `run_test_paced` returned no error, so the test runner printed
  `===TEST_END test_rs_i8042_mouse` with a tail other than `error=`: `exit=<n>`,
  none, or one it cannot parse. A stall, a ceiling or a runner error ends in
  the `STALLED` message instead.
- **The guest stopped reading mid-burst.** 876 injected is the four lead-in
  packets plus 872 of `BURST`'s 1000. The shortfall, 4, equals `MOUSE_LEAD`:
  the host always refills to `arrived + MOUSE_LEAD`, so any guest that stops
  reading mid-burst leaves exactly that many unread.
- **Not the guest's `RUN_CEILING`.** The test took 17 s, and the ceiling is
  60 s from `===I8042_MOUSE_READY===`.
- **The capture that would tell is not kept.** The message carries neither
  the guest's stdout, the kernel's serial window, nor the exit code, and
  `i8042_mouse` never reads `exit_code` before this count. So the log's missing
  `mev done` line says nothing. The boot is `Profile::Metal`, whose 16550 is
  the console on stdio, so it writes no `uart-*.log`. No `Boot parameter:` line
  in the run's kept serial logs carries `i8042-trace`.

Two paths in the tree end the guest this way, and nothing captured tells
them apart:

- **The guest's own end rule, met by a misframed packet.**
  `tests/toyos-rust-tests/src/bin/i8042_mouse.rs` exits 0 on the first event
  without the right button after one with it. `toyos_ps2::mouse`'s decoder
  resets to a head on any gap between bytes longer than `PACKET_GAP_NS` (5 ms).
  Measured on the host by feeding the burst's bytes to `MouseDecoder`: one such
  gap between a −1 packet's head `0x18` and its `dx` `0xFF` takes `0xFF` as a
  head. The decoder emits `buttons=0x07`, discards the next packet's `0x01 0x00`,
  and then emits the following −1 with `buttons=0x00`. That is a press and
  release of the right button. Whether a gap that long in guest time lands
  inside a packet on this host is not measured.
- **A non-zero exit**, which this test does not read.

## Exit condition

The shortfall refusal (`i8042_mouse` in `tests/toyos.rs`) carries
`result.stdout` and `result.exit_code`, so the next sighting is not blind; the
mechanism is named, and a deterministic test is red on it. Then this file and
its `src/redlist.rs` row are deleted.

## Owner

The i8042/input path, held by the orchestrator.
