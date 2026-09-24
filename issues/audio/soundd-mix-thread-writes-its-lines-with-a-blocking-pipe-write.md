---
status: open
kind: defect
opened: 2026-09-25
---

# soundd's mix thread writes its lines with a blocking pipe write

soundd's stdout and stderr are a pipe init made to `logd`, and `say!`
(`userland/soundd/src/main.rs`) is a blocking `write_all` on it. The mix
thread runs in the RT band and calls `say!` on its period path: the stats
line every `STATS_INTERVAL_NANOS`, client connect and removal, suspend and
resume, the bounded idle-wake lines (`userland/soundd/src/mix.rs`), and the
virtio driver's stream start, stop and device-event lines it reaches through
`Backend` (`userland/soundd/src/virtio.rs`). A full pipe parks the writer, so the RT
thread's wait on `logd` is bounded only by the pipe's size.

The bound, derived and not observed: the pipe is `PIPE_SIZE`, 2 MiB
(`kernel/src/pipe.rs`), and soundd's pipe is its own (init makes one per
program). A streaming mix thread writes one stats line of about 223 bytes
every 2 s (the format filled with a gate-A run's values, measured with `wc -c`),
so where that line is the whole of its traffic the RT thread waits on `logd` only after `logd` has read nothing from soundd
for about five hours of playback. A `logd` parked forever — on a `/log` device
that stopped answering, say — still reaches it, and audio stops then.

Not measured as a harm: gate A on `audio_tone_load` compared this tree against
`origin/main` in one session, three runs each, and the branch was no worse
(the table is on #492, which moved soundd's output to the pipe).

Exit: nothing on the RT thread can wait on logging — either the mix thread
emits nothing, or its writes are `write_nonblock` with the refused lines
counted and the count said on the next line that gets through.
