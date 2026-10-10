---
status: open
kind: tooling
opened: 2026-10-10
---

# A suite that builds its own sysroot can wedge on its own locks

`src/buildlock.rs` orders a sysroot key's lock before the worktree build lock,
and takes the key's lock with the worktree lock put down. One run of the guest
suite (`cargo test --test toyos-build`, 12 wide) on a head whose sysroot no
earlier build had made held both the other way round, inside one harness
process, and stopped:

- at 11:11:53 every worker waited on `building sysroot 697f7bee2b30245e`, and
  one built it;
- at 11:14:26 the waiters acquired it in turn, one began `clean crate targets
  against changed external deps` and waited for the build lock exclusive,
  `held by other builds in this tree`, while workers that held it shared for a
  test binary waited on `using sysroot 697f7bee2b30245e — held, but the holder
  left no readable note`;
- 450 s later six tests (`https_fetch`, `iommu_virtio_platform`,
  `machine_shutdown`, `nested_nmi_is_loud`, `netstack_streams`,
  `netstack_streams_e1000e`) had not started, `lsof` showed the key's lock file
  open in the harness alone, and the run was stopped. The same head, its
  sysroot made, ran 50 of 50 green in 49 s.

`cargo run -- --ci host` ran beside it in the same worktree; whether it took
part is not established.

**Exit condition**: no path holds the worktree build lock while it waits on a
key's lock, and a suite started on a head whose sysroot is not yet made
finishes.
