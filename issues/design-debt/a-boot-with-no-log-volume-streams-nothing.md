---
status: open
kind: defect
opened: 2026-09-08
---

# A boot with no `/log` streams nothing, and that is backwards for the bench

`userland/logd/src/main.rs`'s loop offers the record stream only what it has
already written to the volume, and it reaches that offer through
`let Some(v) = volume.as_mut() else { continue }`. So a boot with no log
partition, or one whose volume `policy::fate` has given up on, streams not one
record — the second sink is a mirror of the first and dies with it.

That is right for the rule it comes from (the file is the sink of record and the
stream may never cost it a line) and wrong for the machine the stream was built
for. On the bench's ThinkPad a dead stick is precisely when the cable is the
only channel left, and it is the case where the stream is worth most.

The exit condition is a decision about what `logd` offers when there is nothing
to write to: the records it read, or nothing. Whichever it is, the ordering rule
the stream rests on — a line reaches the file before it reaches the wire — has
to be restated for a boot where there is no file, because today it is what makes
the answer "nothing" by construction.
