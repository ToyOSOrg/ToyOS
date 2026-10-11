---
status: open
kind: defect
opened: 2026-10-11
---

# The AMD bench answers on two HDA controllers, so the kernel refuses both and soundserver plays on the null sink

On the AMD bench's boot at `fb03703a1` (PR #852, the log in
issuecomment-6107031099 and issuecomment-6107031259) two HDA controllers have
a live link, `03:00.1` (`1002:1637`) and `03:00.6` (`1022:15e3`), each
`statests=0x0001`. `kernel/src/drivers/hda.rs` refuses a machine with more
than one by name: `hda: 2 controllers answer on this machine and choosing
between them means walking their codec graphs, which is the driver's — refused
by name, no HDA audio`. The supervisor then finds no `hda-audio` for
soundserver, which presents the null sink.

So the bench has no audio, and `test_rs_hda_client_stall`, which the
`hda_client_stall` row judges, exits 101 there: its `await_suspended` reads
the stream's state as `idle` for 5 s after its only client closed, and the
job's premise is a device stream that suspends. `test_rs_audio_tone`,
`test_rs_audio_idle_suspend` and `test_rs_null_sink_client_exits` exited 0 on
the same boot, on the null sink.

Which of the two carries the laptop's speakers is unread; by its vendor
`1002:1637` is the GPU's display audio.

Owner: the HDA stage of `issues/every-driver-is-still-in-the-kernel.md`, since
the refusal names choosing as the driver's work and that stage moves the
driver to soundserver. That a row needing an HDA controller runs on a bench
without one usable is `issues/a-metal-row-names-no-hardware-and-a-bench-without-it-reds.md`'s.

## Exit condition

The driver walks both controllers' codec graphs and plays on the one whose
graph reaches an output pin, and an AMD bench boot reads soundserver on HDA
and `test_rs_hda_client_stall` exiting 0.
