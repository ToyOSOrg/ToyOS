---
status: assigned
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

**The other eight boots**, which had no such row and set no APST:

- six met the slow mode and stayed in it through `persist`;
- one ran fast throughout (round 2 of main with the row);
- one left the slow mode by itself. Round 3 of main with the row began
  `persist.write_4k_fsync` slow and ended it fast (p50 2.20 ms, p90
  6.41 ms, mean 3.76 ms, about half its samples slow), and every durable row
  after it was fast (`persist.append_4k_fsync` p50 1.12 ms, p90 1.15 ms).

So a boot that met the slow mode left it unprompted in one of seven, and
left it across the idle row in four of four. The slow-write record already
holds a boot that changed mode mid-run the other way.

Evidence:
https://github.com/ToyOSOrg/evidence/blob/7efbe1295aa54370b213719b4d4f460e7a103554/ab/README.md
Its tables class round 3 of main as fast, because their rule is a p50 over
3 ms; the row's own p90 says otherwise.

**What is not separated.** Three things could have ended the mode in those
four boots: the driver's Autonomous Power State Transitions, the 80 s of
idle, or nothing at all, as in the one boot above. The image carried the
first two together.

**Assigned** to the measurement that finishes PR #1131: three boots per
arm on the T14, one arm with the idle row and no APST, the other with both.

**Exit**: counts, not one boot. Of the boots of each arm that begin
`persist` slow, how many leave the mode across the idle row, set against
one in seven unprompted. The slow-write record is updated with what the
counts show, and with "not separated" if they do not show it.
