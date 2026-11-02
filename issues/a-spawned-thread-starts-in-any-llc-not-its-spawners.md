---
status: assigned
kind: defect
opened: 2026-11-02
---

# A spawned thread starts in any LLC, not its spawner's

Spawn placement (`CpuHandles::place`, from `kernel/src/sched/driver.rs`'s
`placement`) ranks CPUs by queue, then by whether they sleep with no kick
coming, by busy SMT siblings and by wake depth, and breaks ties by a rotation
over the whole machine. It never weighs the LLC of the thread doing the
spawning. A thread that works with the thread that spawned it, sharing its
address space and often its data, therefore starts split from it across LLCs
whenever the rotation lands in the other one. Nothing brings it back
afterwards while neither side parks often.

On the AMD bench (LLCs 0–7 and 8–15), ab9: six `benchcase` boots, base #1117
`c9f88d36e` and head #1129 `cebbdedd5`, each with a per-round diagnostic
([judgment](https://github.com/ToyOSOrg/evidence/blob/6b481f7d84081c39c3d9ee8160ea1bda0d1bb687/pr-1129/r8/judgment.md),
[rows](https://github.com/ToyOSOrg/evidence/tree/6b481f7d84081c39c3d9ee8160ea1bda0d1bb687/pr-1129/r8/runs)):

- `pipe.throughput`'s writer is a new thread each round. At 16384 its CPU
  followed the rotation 14, 15, 0, 1, 2, 3, 4, 5 in every boot of both arms,
  whatever LLC the reader, osbench's main thread, was in.
- Every round whose writer and reader ran in different LLCs read slow, in both
  arms and with no exception: about 2.0, 3.5 and 4.8 GB/s at 4096, 16384 and
  65536 bytes per write, against about 5.0, 8.4 and 10.0 in one LLC.
- So the row's p50 reports which LLC the reader happened to sit in: 2 of 8
  rounds split in two base boots and 6 of 8 in the third.

Open observation: one same-LLC round (base boot 5, round 1, reader cpu13 and
writer cpu15) read 5.6 GB/s, as did its warmup on 13 and 14. Its record shows
no move, no other process on those cores and nothing lost. It is not explained.

Assigned to the change that places a spawned thread on a free core in its
spawner's LLC first.
