---
status: assigned
kind: defect
opened: 2026-10-28
---

# A counters read within 10 ms of another returns the older reading as new

`SYS_COUNTERS` reads every CPU in a round: the reader kicks each CPU and
waits for its answer (`kernel/src/counters.rs`). A read issued within
`JOIN` (10 ms) of the last round's issue waits on that round and is served
its values. Nothing in the reply says so. The reader gets a reading taken up
to 10 ms before it asked, and two reads 5 ms apart can get one reading twice.

#1073's group-start diagnostic read the counters around each group's 5 ms
`cpu.clock` window. 13 of its 17 windows came back with before equal to
after on every CPU (`ToyOSOrg/evidence` branch `pr-1073`, `r6/`). osbench's
shipped instruments now take each window from the readings' own counter
stamps and refuse a CPU whose readings are not the row's
(`osbench_rules::window`). That makes the defect visible, not absent: a
caller that does not compare stamps still reads an old reading as current.

Owner: the idle track's follow-up
(`issues/idle-cpus-enter-the-c-states-cst-names.md`), in
`kernel/src/counters.rs` and the `toyos_abi::counters` record.

**Exit**: a read never returns a reading older than its own call as
current. Either it waits for a round issued after it, or its record says it
was served an earlier round and when that round was issued; the ABI test
and a kernel host test hold whichever is chosen. osbench's window rule
stays as the reader's check.
