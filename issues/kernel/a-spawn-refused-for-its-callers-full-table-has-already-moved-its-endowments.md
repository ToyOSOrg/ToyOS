---
status: open
kind: defect
opened: 2026-10-01
---

# A spawn refused for its caller's full table has already moved its endowments

`sys_spawn` (`kernel/src/syscall/proc.rs`) installs the new child's `Process`
handle in its caller's table after `loader::spawn` returns, which is after
`PendingHandles::commit` (`kernel/src/loader/start.rs`) moved the endowed
handles out of that table and the child landed. When the install finds the
table full, the spawn kills the child and answers `ResourceExhausted`: a
refusal after the caller's handles left, and a child that may have run user
code before its retire landed. Every other refusal of a spawn leaves the
caller's table as it was.

The moves free one slot per endowment, so with endowments it takes another
thread of the caller filling the table between the commit and the install;
with none, a caller whose table is full when it spawns. Neither is measured.

*Exit*: the child's handle goes into the caller's table in the commit's hold,
with the room checked before anything moves; a guest arm spawning from a
full table reads `ResourceExhausted` and no child started.
