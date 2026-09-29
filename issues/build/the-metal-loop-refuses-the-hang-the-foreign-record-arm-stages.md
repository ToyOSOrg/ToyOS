---
status: expected-red
kind: tooling
opened: 2026-09-29
---

# The metal loop refuses the hang the foreign-record arm stages, so `blackbox_foreign_record` never reaches its judge

`blackbox_foreign_record`'s metal arm flashes `tests/jobcase` armed with
`blackbox-foreign-identity`: the stop seals its record under an identity one
bit away from the stick's. The pass after the reset clears that record as
another image's, so no record of this image's boot reaches it, and the
attempt count reads the boot as a hang and hands the machine back. The QEMU
half (`power::blackbox_foreign_record`, `tests/common/power.rs`) says that
second pass "also hands the machine back, and that is right". `toyos-metal`
(`src/metal.rs`) returns `Refusal::HungWithoutARecord` on any `loader.log`
carrying `bootlog::HUNG_WITHOUT_A_RECORD`, before the arm's judge runs and
whatever the image is armed with.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1):

```
toyos-metal: the last boot of this image was handed the machine and never reported: no panic, no fault and no deliberate handover, which is a hang. This pass refused to boot the same kernel again and gave the machine back, so the machine is free and nothing needs a hand — but this boot measured no test, and why that kernel stopped is the boot before it
  FAIL foreignrecord: toyos-metal exited exit status: 1
  FAIL blackbox_foreign_record: toyos-metal exited exit status: 1
```

The pass after the reset
(`/Users/jan/Dev/jan/toyos-metalmain/target/metal/foreignrecord/loader.log`)
carries the line the judge asks for, then the hang:

```
Black box: 0x8000000 held a DONE record another image left in this memory ([63, 12, b3, ...], and this stick is [9c, 12, b3, ...]), armed at 2026-09-29-103651. It has been cleared and this pass boots its kernel
Boot attempts: this image has had the machine 1 time(s) without reporting; now 0
Slot A: its image 5d1a1666... died on its last boot, so no pass boots it again until an update replaces it
Boot attempts: the previous boot of this image never reported; the machine is handed back
```

The kernel's own boot was sound: `Boot: complete (1171ms)`, then init's
`power: the machine stops`.

## Exit condition

`toyos-metal` hands a boot armed with `blackbox-foreign-identity` to its judge
instead of refusing the hang it stages, and a T14 run of
`blackbox_foreign_record` reaches `record another image left in this memory`
in the judge; then its row in `src/redlist.rs` and this file are deleted.
