---
status: open
kind: tooling
opened: 2026-09-26
---

# QEMU drops console output that the harness is slow to read

The harness reads each guest's virtio console from QEMU's stdout
(`-chardev stdio,id=cs0` behind a `virtconsole`, `tests/common/qemu.rs`).
QEMU makes that stdout non-blocking (`qemu_chr_open_fd`, `chardev/char-fd.c`,
v11.1.0). Its `virtconsole` then drops whatever a full pipe refuses:
`flush_buf` in `hw/char/virtio-console.c` throttles the port only
`if (!k->is_console)`, and its own comment says it is "silently dropping
console data on EAGAIN". It completes the transmit buffer either way, so the
guest cannot tell a line was lost.

Seen on `wt/toyos-logtrack`, where `log_stream_stalled_reader` puts about a
mebibyte of program lines through the console after its flood:

- A passing run printed `flood 001999`'s first 460 bytes followed directly by
  the whole of `flood 002159`, on one console line. Every entry the guest's
  queue holds is a whole line, or a 1024-byte piece of one, so a fragment of
  that length is bytes the host dropped.
- A hung run printed a 555-byte fragment of `flood 005384`. After it came
  nothing that `logd` put on the console between 5.6 s and 28 s, including
  the runner's `===TEST_END===`, while `/log` holds every one of those lines.
  The harness waited for the marker until its ceiling. The host's load
  average was 40–50.

Across 8 runs that day, 2 hung this way. Any test that waits for a line on a
virtio console under host load is exposed in the same way; the flood only
makes it likely.

## Exit condition

The console chardev the harness reads cannot refuse a write — for example a
file chardev the harness follows, or a `virtserialport`, which QEMU throttles
instead of dropping. Shown by `log_stream_stalled_reader` green in 20 of 20
runs beside a full nightly.
