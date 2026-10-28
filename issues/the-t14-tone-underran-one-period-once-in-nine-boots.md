---
status: open
kind: defect
opened: 2026-10-28
---

# The T14 tone underran one period once in nine boots

`hda_tone` reds when soundserver fills a period the tone client had not
covered (`tests/common/audio.rs`'s `tone_on_metal`). On one T14 `testcases`
boot it read `underruns=1 starve_max=1` in the stream's first 2 s stats
window (11.772–13.773 s of a stream that closed at 14.977 s): one isolated
period, somewhere in that window. The second window and `hda_tone_resampled`
in that boot read 0.

Distinct T14 boots judged for this row:
- 7 of PR #1074's branch. Four are stick boots of earlier revisions on older
  mains, and three are NVMe boots. The NVMe three include the red one (head
  094323f29) and the same image booted again 24 minutes later.
- 2 of main 370d7d9f6 alone, NVMe.

If both trees underran at one rate, the red would land on a branch boot 7
times in 9 (3 in 5 counting NVMe boots alone), so the counts attribute it to
neither.
([landing boot](https://github.com/ToyOSOrg/evidence/tree/3cb575aadca9e85f33b4c5690c7e1ea4ca15822f/r8),
[every boot's judgement](https://github.com/ToyOSOrg/evidence/tree/9d0ed9325f33f34ba7714fe68ca1261129c27f0a/r9))

There is no candidate cause. The samples cross a shared-memory ring.
soundserver counts an underrun only once a client is streaming, that is once
it has delivered (`ClientStream::is_streaming`). The one pipe on the path is
the signal bell, and after delivery `signal_clients` (`system/soundserver/src/mix.rs`)
rings it once per mix wake. It drops every error but `Gone`, and a bell
refused for memory would be dropped too. But neither tree can refuse that
write for memory in a stream this short:
- on main 370d7d9f6, a pipe holds its page from its first write;
- with #1074, a pipe that has held a byte needs another frame only when its
  held bytes cross a frame's 4096 bytes (`kernel/pure/pipe.rs`'s `wants`, and
  `consume` restarts a drained pipe at its frame's start). That takes about
  4096 bells, and this stream rang at most about 1400 (one per mix wake,
  871 + 515).

**Owner**: soundserver and the hda driver.

**Exit**: soundserver's underrun record names the period, how late the
client's fill was and how late the device's completion was. A recurrence
then names its cause, and the fix lands. The record does not close on a
count of clean boots. At the observed 1 in 9, 26 consecutive clean boots
are needed to exclude that rate with 95 % confidence, and about 300 to
bound it under 1 in 100.
