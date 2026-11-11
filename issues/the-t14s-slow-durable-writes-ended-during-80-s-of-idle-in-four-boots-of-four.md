---
status: open
kind: finding
opened: 2026-11-11
---

# The T14's slow durable writes ended during 80 s of idle in four boots of four

The T14's drive runs its durable writes several times slower in some boots
(`issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md`),
with no measured cause. In the A/B of PR #1202 and PR #1131 on 2026-11-11,
all four boots of #1131's image (`24f5de778`) began `persist` in the slow
mode and left it during one row:

- `persist.write_4k_fsync`, the first durable row: 5.83 to 6.21 ms.
- then `persist.write_4k_fsync_after_idle`, which sleeps 10 s before each of
  its eight samples;
- then every durable row after it at the fast mode's speed, in every boot:
  `persist.append_4k_fsync` 0.93 to 1.08 ms, against Linux's 0.79 to
  0.92 ms.

The other two arms had no such row and no such change: a boot that began
slow stayed slow through `persist` (six of eight boots; the other two began
fast).

Evidence:
https://github.com/ToyOSOrg/evidence/blob/7efbe1295aa54370b213719b4d4f460e7a103554/ab/README.md

**What is not separated.** That image differed from the others in two ways
at once: its driver set Autonomous Power State Transitions, and its
`persist` idled 80 s. Either could be what ended the mode. The measurement
that finishes PR #1131 carries an arm with the idle row and no APST.

**Exit**: a T14 boot with the after-idle row and no APST shows the mode
ending there or not, and the slow-write record is updated with which of the
two it is.
