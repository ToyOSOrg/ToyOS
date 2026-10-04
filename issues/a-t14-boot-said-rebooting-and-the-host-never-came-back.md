---
status: open
kind: defect
opened: 2026-09-25
---

# A T14 boot said `Rebooting.` and the host did not come back within 420 s

T14 run 132, `lanswapcase` on `wt/toyos-logd` `b9435b98`. `toyos-metal` gave up
with "the machine did not come back within 420 s". The stick's `/log` file,
read through its FAT chain (109 clusters, 55676 bytes, the size its directory
entry records), is whole and ends:

```
[60.001] test-runner: the job list ran past its bound, and the job it was inside is test_rs_lan_swap_hold (60000 ms)
[60.033] stop: 5 of 5 userland thread(s) stopped across 8 cpu(s) in 0 ms of a 2010 ms budget ...
[60.033] Rebooting.
```

So the kernel ran the whole quiesce and made its last word durable; what
follows `Rebooting.` — the black box seal, `xhci::seal_shut`, the reset
register — writes nothing the stick keeps, and the black box is read only by
the next boot of a ToyOS loader, which did not happen. Whether the reset was
written, whether firmware came back, and whether the host's Linux came up
without its network (the host is reached over the same I219 this boot's netd
left faulted) are not separable from anything on the stick. A 454-line copy of
the same file that ends at 35.585 s was taken for the whole log; it is a
truncated read, not the file.

**Exit condition**: a metal run whose machine does not come back says which of
those three it was — the owner's panel photograph, the host's own boot log for
the window, or the black box read by a ToyOS boot — rather than "did not come
back".
