---
status: expected-red
kind: tooling
opened: 2026-09-28
---

# `quiesce_leaves_the_volume_whole` passes only when the refused flush closes inside the stop's `PARK`, which a host stall takes away

The verdict needs `fsync: … durable on attempt 9` on the console before
`Syncing filesystems...`. In other words, the `quiesce-fsync-refuse` ladder has
to close before `quiesce::stop` spends `PARK`. The
kernel's own `const` assert in `fat32_adapter::mirror_refuse` only covers the
parks: 1270 ms of `RETRY_SOONEST` doubling against `PARK`. The I/O of nine
attempts, and any time the guest is not running, are not covered by anything.
`PARK`'s own doc says a thread can outlast it and that the record then names
the shortfall. So the test asserts an outcome the kernel does not promise, and
under TCG the guest clock runs with the host's.

The one red, the Fast tier for PR #562 at `2a9c77ee`:

```
refusals 1..8 at 630 643 658 681 723 809 974 1304 ms   (the nominal ladder)
{0.649 init} init: power: the machine stops …
[kernel 2.703 cpu0] Syncing filesystems...
[kernel 2.705 cpu1 tid=1] fsync: /log/quiesce-fsync.bin durable on attempt 9 after 2073ms
stop: 6 of 7 userland thread(s) stopped … in 2037 ms of a 2010 ms budget
```

Attempt 9 was due at about 1944 ms (a 640 ms park after 1304). It closed at
2705 ms, two milliseconds after the stop's own deadline wake, which fired
27 ms late. A single stall of the whole guest from before 1944 ms to past
2676 ms explains both late wakes firing together. Nothing in the guest was
waiting on the other. Nothing here is PR #562's doing either: on that branch
the kernel paths this boot runs (`quiesce.rs`, `block.rs`, `fat32_adapter.rs`)
are the same as on `main`. The one change to `quiesce_fsync.rs` removes a
deadline the guest only reached on a hang.

## Exit condition

The verdict no longer rests on guest time. One way: a stop whose budget ran out
over the parked update is read as its own outcome, with the volume judged whole
or not by the checker. Another: the actuator holds the ladder open until the
stop has swept, rather than for a fixed ladder of parks. Then this file and its
`src/redlist.rs` row are deleted.

## Owner

`tests/common/volumes.rs` `quiesce_leaves_the_volume_whole`, the
`quiesce-fsync-refuse` actuator. Nobody holds it.
