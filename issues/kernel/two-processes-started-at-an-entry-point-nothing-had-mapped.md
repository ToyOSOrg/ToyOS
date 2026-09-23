---
status: open
kind: defect
opened: 2026-09-22
---

# Two processes started at an entry point nothing had mapped, and the boot went with them

Seen once, on CI run `35760805511` attempt 1, shard `guest (9)`, on PR #466's
head `0a6b7da5` — which is `cf715c49` plus a USB/xHCI branch. The name that
carried it is `log_reserve_window` (`--smp 8`, actuator `log-nested-reserve`);
the harness's verdict is the workload's name and not the cause:

```
FAIL log_reserve_window: [qemu] Init process crashed during boot:
[kernel 6.047 cpu5] SEGFAULT tid=0: execute unmapped address at 0x10000042950
[kernel 6.047 cpu6] SEGFAULT tid=0: execute unmapped address at 0x1000001ff80
[kernel 6.047 cpu6]     0x1000001ff80  _start+0x0
[kernel 6.047 cpu6]   Page walk for 0x1000001ff80 [PML4=0x3028000 PCID=4 PML4[2] PDPT[0] PD[0] PT[31]]:
[kernel 6.047 cpu6]     PML4E: 0x0000000000000000 P=0 W=0 U=0
[kernel 6.047 cpu6] exit: test-runner pid=6 code=-1 cpu=4471ms
[kernel 6.047 cpu5] exit: soundd pid=5 code=-1 cpu=4553ms
```

**What the words say.** Two different processes, on two CPUs, in the same
millisecond, each faulting on the *first instruction* of its own image, each
with its address space's **PML4 entry** for the user half absent — not a leaf
page missing under demand paging but the top-level entry, so nothing of the
image was reachable. Both had run: `cpu=4471ms` and `cpu=4553ms` are their own
spent time, and both stacks still hold their argv (`/system/bin/test-runner`,
`/system/bin/soundd`). Each carries a distinct PCID (3 and 4) and a distinct
CR3, so this is not one address space seen twice. The registers read
`rip=_start+0x0`, `rsp=0xffffffffd0`, `r12=<entry>` on both — the shape of a
thread being started, not of one that ran and returned into nothing.

`fs:[0] = 0xffff5fff90 (expected 0xffff5fff90)` on both, so the TLS the kernel
installed was still readable when the fault was reported.

**What is not established.** Whether the mapping was never made, was torn down
under them, or was made and not visible to the faulting CPU (both PCIDs are
young, and a stale CR3/PCID would read exactly like this). The record has no
`spawn:` line for either process in the captured window, and the capture is the
first 80 of 86 lines the harness kept.

`issues/kernel/echo-faulted-after-the-fault-arms.md` is the closest relative —
a spawned process faulting immediately, once, under host load — and
`issues/kernel/a-spawn-of-echo-was-refused-with-an-error-nothing-names.md` is
the *other* name on the same CI run whose child would not start at all. The
three may be one defect; nothing here shows they are.

**What it is not.** It is not PR #466's, as far as four sessions each way can
say: on that head, four full fast-tier `cargo test` runs on the dev host
(2026-09-22, sessions of 347 tests each) had `log_reserve_window` green 4/4;
with the whole branch reverted onto `cf715c49` as a checked patch, four more
sessions of the same suite had it green 4/4. The branch's kernel diff outside
the USB paths is additive (`mm/dma.rs` gains two functions nothing else calls,
`arch/tlb.rs` changes a doc comment and a visibility) and touches no part of
spawn, address-space construction or TLB shootdown. Neither arm reproduced it,
so neither arm explains it: 0/4 against 0/4 is an instrument that does not
reach this, not an acquittal.

**Exit.** A capture that says which of the three readings is true: the guest
kept alive at the fault (`BootOptions { qmp: true, .. }`, `info registers -a`
for what the *other* CPUs hold) and the `spawn:` line for the faulting pid in
the same log, on a host loaded the way a CI shard is — eight guest vCPUs on a
four-CPU runner. Until then the rate is one guest in one shard of one run.
