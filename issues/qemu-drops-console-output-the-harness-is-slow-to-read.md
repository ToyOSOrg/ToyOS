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

**`log_stream_stalled_reader` no longer reads that channel.** It boots with
`BootOptions::console_file`: the virtio console's output goes to a regular
file the harness follows and its input through a FIFO, so no write is refused.
Twenty runs of it after that change: 18 green; one red was the kernel's TLB
shootdown panic on a starved vCPU
(`issues/a-shootdown-panicked-on-a-cpu-the-host-starved.md`), which
killed a `logd` reader thread so it was never let go, and one had `logd` let
none of the eight stalled readers go in 90 s while every host guest slot was
held by other worktrees, green alone. Neither lost a console line. Once
`logd` synced every round the test's stimulus was too small for its readers
to stall, and its flood was made four times wider; twenty runs after that, at
host loads of 3.9 to 9.0, were 20 green. Every other test still reads the
stdio console.

**`blockd_serves_partitions` is a sighting of the same loss.** In `650-libcllvm-whole.log`
(`wt/toyos-libcllvm`, load average 66–74) test-runner printed `blockd_io: PASS bench` at
35.962 s and the bench's process exited `code=0` at 35.978 s, and `===TEST_END
test_rs_blockd_io exit=0===` never reached the harness, which waited 3559 s until the run was
ended by hand. That boot read the stdio console.
Every userland line stops at 35.962 s while the kernel's own ten-second `sched:` and `PMM:` lines
go on, and user `mmap` held rises from 23 to 30 between 74.9 s and 3026.8 s, so a process kept
running through the silence; the capture cannot tell a dropped marker from a `logd` that stopped
forwarding. The tree moved `rust` from `aca5f527f` to `9151571ca`, which changes std's exported C
`malloc`, `free` and `realloc` in every Rust guest program, so reading it as this loss rests on
that change being off its path. Owner: the orchestrator.

## Exit condition

The console chardev the harness reads cannot refuse a write — for example a
file chardev the harness follows, or a `virtserialport`, which QEMU throttles
instead of dropping. Shown by `log_stream_stalled_reader` green in 20 of 20
runs beside a full suite.

**`log_stream_stalled_reader` is deleted**, as a flaky test is, on the two
reds recorded here: `96763794e` took it out, and `cc291947e` then deleted
`BootOptions::console_file`, which only it set.
