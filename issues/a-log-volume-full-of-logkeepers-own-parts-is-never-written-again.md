---
status: open
kind: finding
opened: 2026-11-10
severity: [data-loss]
---

# A log volume full of logkeeper's own parts is never written again

`logkeeper` deletes its old files in one place, `sweep`
(`system/logkeeper/src/store.rs`), which runs from `Volume::open`, before the
boot's first create, and from `Volume::rotate`. It keeps at most
`MAX_LOG_FILES` files and counts files, not bytes. A part the volume refused
is opened again by `Volume::reopen`, which does not sweep, and a refused part
never fills, so it never reaches a rotation.

So where `/log` has no room and holds fewer than `MAX_LOG_FILES` of
`logkeeper`'s files, nothing frees any: the open's sweep deletes nothing, the
create or a later write is refused, and every retry is refused the same way
for the rest of the boot. A later boot whose first create is refused adds no
file, so it stands where this one did. The boot's log is then in the replay
and on the console only, which `logkeeper` says, and the stop is not marked.

With `MAX_LOG_FILES` or more files the open's sweep deletes only as many as
the count is over by, one where there are sixteen, and a rotation one more.
That frees what the retired files held and no more, so on a volume otherwise
full the boot is refused once it has written past that, and from there stands
as above.

Sixteen parts of `MAX_LOG_BYTES` are under half of the smallest log volume, so
this needs something else to have filled it: the bootloader's files, a
diagnostic dropped beside the logs, or a volume smaller than the build makes.

Read from the code. The refusal and the doubling retry were seen on the guest
runner on a volume a filler file had filled (run 42133377735's stalled row);
no run has filled a volume with `logkeeper`'s own parts.

**Exit**: a part refused for want of room frees room from `logkeeper`'s own
older files by `retire`'s order before it is asked again, or the record of a
ruling that a full log volume is the owner's to clear.

Owner: `system/logkeeper`; unheld.
