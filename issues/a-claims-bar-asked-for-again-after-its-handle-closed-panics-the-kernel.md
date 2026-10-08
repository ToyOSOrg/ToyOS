---
status: open
kind: defect
opened: 2026-10-08
---

# A claim's BAR asked for again after its handle closed panics the kernel

`pcidev::bar_object` (`kernel/src/pcidev/mod.rs`) makes one object per memory
BAR and keeps it in the claim's binding, to answer every later
`SYS_DEVICE_BAR_MAP` of that BAR with "the same object every time". The object
it keeps is counted by its handles like any other: when the one handle the
first answer installed closes, `HandleEntry`'s drop
(`kernel/src/object/handle.rs`) retires it. The binding still holds it, so the
next request for that BAR hands the retired object to `ops::install`, and
`HandleEntry::new` asserts against exactly that. The holder of a live claim
takes the machine down with three ordinary calls: ask, close, ask.

The same object is retired without a close when the first install is refused
for a full handle table: the entry built for it is dropped. Read from the
code, not run.

**Measured** at `06bbc236d`, on `tests/testcases` under QEMU with the virtio
NIC as the claimed function: `cargo test --test toyos-build -- bar_map_again`
exits 1 on

    PANIC: panicked at src/object/handle.rs:108:9:
    a handle to a retired SharedMem (koid 197)

after the job's line `bar_map_again: BAR 4 asked for and its handle closed;
asking again`.

No metal row was run: the T14's one function a job can claim is its I219, and
a red here is the bench's machine in a panic.

**Exit condition**: a BAR asked for again on a live claim, after every handle
to the first answer has gone, is answered with a handle that maps or with a
refusal, and `bar_map_again` is green; the arm where the first install is
refused for room is read by a test as well.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.

**Its test is deleted**: `a86cafa06` took `bar_map_again` out, and
`git revert a86cafa06` brings it back.
