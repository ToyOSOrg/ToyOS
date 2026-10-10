---
status: open
kind: defect
opened: 2026-08-19
---

# A deferred release can finish after the syscall that caused it has returned

`object::drain_zero_handles` (`kernel/src/object/mod.rs`) takes the whole queue
and clears `ZERO_PENDING` **before** it runs a single hook:

```rust
let batch = {
    let mut queue = ZERO_QUEUE.lock();
    ZERO_PENDING.store(false, Ordering::Release);
    core::mem::take(&mut *queue)
};
for object in batch {
    object.run_zero_handles();
}
```

So "the queue says empty" is not "the work is done", and the CPU that *queued*
an object is not guaranteed to be the one that releases it. The drain runs at
three sites — syscall exit, `do_schedule` entry, the idle loop — and any of the
other two, on any other CPU, can take a batch out from under the syscall that
filled it. That syscall then reaches its own drain site, is told the queue is
empty, and returns to userland with its objects still unreleased.

**Measured 2026-08-19 on `wt/toyos-fdleak` at `8e9f851`**, `tests/testcases`,
two CPUs, TCG, one binary in the guest. `handle_lifetime`'s holder makes eight
io_uring rings (16 MiB) and is killed; the killer reads `SYS_SYSINFO` eight
times back to back straight after `wait` returns. The deficit against the
pre-spawn reading, in megabytes, over those eight reads:

```
round 1  [12, 10, 10,  8,  6,  6,  6,  6]
round 3  [12, 10, 10, 10,  8,  6,  4,  2]
round 5  [10, 10,  8,  6,  4,  2,  2,  2]
round 9  [14, 12, 10, 10, 10,  8,  6,  4]
round 13 [14, 14, 12, 10, 10, 10, 10,  8]
```

Ten of twenty rounds decayed like that; the other ten read zero on the first
try. It is a 2 MiB staircase — one ring page at a time — which is the other
CPU working through the batch while this one reads.

**Nothing is lost.** Over the same twenty rounds free memory returned to its
starting value every time; the drift against the round-0 baseline was zero at
every round. A kernel trace confirms the shape from the other side: with
`RingRef::drop`, `drain_zero_handles` and `SYS_PROCESS_KILL` logged, all eight
`RingRef` frees land in a `batch=9` drain that runs *after* `kill_process` has
returned, and a second CPU was caught taking a batch mid-kill —

```
[cpu0] KILLPROBE enter target=15 t=552074985
[cpu1] ZQPROBE drain batch=1 t=552533113
[cpu0] KILLPROBE done  target=15 t=554353621
```

## A third witness, and it is not a free-memory verdict

`handle_kill_policy` reds on the same mechanism through a completely different
instrument — the per-kind object census, not `SYS_SYSINFO`. Seen on this branch
in a full twelve-wide suite, 2026-08-19, at `bb6893c`:

```
16 more killed processes left more live objects behind:
  [("SharedMem", 5, 6), ("Process", 6, 7)]
```

One `SharedMem` and one `Process` still alive at the closing reading. That is
the same "the release has not run yet" and not a leak: `SharedMem` is a
`deferred` row released from `ZERO_QUEUE`, and a `ProcessObject` outlives its
table entry until `reap_finished` takes it — which runs from the idle loop
under `IdleProof`, so it is a second asynchrony of the same class with a longer
tail. The census is immune to *another binary's churn*, which is what the
free-memory verdicts are not, and it reds anyway. So the shared boot was never
the common factor between these three names; the release latency is.

**A fourth witness, hosted CI, 2026-08-25.** `handle_transfer` red on run
32876917304 `guest (3)` — the census found one extra live `PipeRead` (2 → 3)
after its deferred-release scenarios, red again in the shard's own alone
re-run inside the same shared boot, on a pull request whose diff is
comments-only and provably byte-identical in code. `PipeReadEnd` is a
`deferred` row whose only release site is `on_zero_handles`, so this is the
recorded mechanism through the census instrument on the hosted shard — the
first sighting of this class off the dev host.

**A witness, PR #564 at `4919fbd7`.** `handle_basic` red at
`tests/toyos-rust-tests/src/bin/handle_basic.rs:305` — sixteen more rounds of
handle churn left one extra live `PipeWrite` behind (`[("PipeWrite", 5, 6)]`),
`PipeRead` unchanged. CI run 33266767478, job 99138099030 reds the same
assertion on `wt/toyos-wv-fs` at `b10c4daf`, green when run alone. `PipeWrite`
+1 with `PipeRead` unchanged is the last round's `drop(write)` still in the
release queue at the second census reading: this issue's defect.
While `handle_basic` is deleted, four of its assertions run in no gate at all —
a closed slot reissued at generation+1, a superset of rights refused, `dup2`
answering generation 0, then 1, and keeping it across a live replace, and a
spent slot retiring with the table exactly one slot smaller — so this issue's
exit brings them back by restoring it.

## A syscall answering the wrong word, 2026-08-20

**The three witnesses above are quantities that settle. This one is not.**
`kill_while_blocked` kills a child parked in a blocking read and asks the peer
end whether it knows; the answer must be `NotFound` and is `Ok(22)`.
The chain is this queue end to end: the victim's handle goes at
`ops::close_all`, `HandleEntry`'s drop queues the object, and the object's
`on_zero_handles` — `PipeReadEnd`'s or `ConnectionEnd`'s — is the only thing
that calls `Held::release` and gives the `PipeReader` back. `pipe.readers` is
what `pipe::try_write` reads, and it is still 1 while the batch is in flight on
another CPU, so the write is accepted into a ring nobody will ever read.

Measured on `e4c2c8ff`, dev host: **2 red of 53** one-name runs, one on each of
the two arms that ask a peer; **4 of 5** with
the syscall-exit drain removed, which stages the same state a stolen batch
leaves. The trace is a kill returning on cpu0 at 0.542 s and the victim's read
end being released on cpu1 at 0.544 s.

**The two arms are one mechanism, and the session that measured it says so.**
`kill_while_blocked` asks the question twice — `kill_while_blocked.rs:152`, *a
pipe whose only reader was killed mid-read still took a write*, and `:178`, *a
connection whose peer was killed mid-read still took a write*, `left: Ok(22)`
`right: Err(NotFound)`. The two reds in 53 were **one on each arm**, and on the
second boot arm 1 had already printed `pipe: the write end learned its reader had
gone` before arm 2 failed: which end the steal catches is luck, not which path is
faster. Arms 1 and 2 are separate children and separate kills, so one passing
beside the other says nothing about either path.

**The whole session, in order, because no one row of it is the rate.** On
`e4c2c8ff` (`main`'s tip, unmodified): 5 one-name runs → 1 red; the `toyos-mixer`
branch at `47892284` with main merged in, 5 runs → 0; then 53 one-name runs → 2;
4 full fast tiers of 272 tests → 0; and **20 one-name runs on a kernel carrying
one `log!` per release → 0**. That last row is worth as much as the first two:
one log line per release closes the window, so the residual between the kill
returning and the peer's write is of the order of a few log lines' work. No count
here is its real rate — a race shows up more often beside 271 other guests than
in a one-test run, and the four quiet full tiers are four, against a first
sighting that was inside one.

**Two things the first filing guessed, and neither survived.** It is not that
"the pipe path publishes the death before the connection path does" — both arms
reded. And it is not new: `git diff 625afce1 e4c2c8ff -- kernel/` is comments,
doc comments and one `#![warn(clippy::undocumented_unsafe_blocks)]` attribute,
no behaviour change anywhere; `ZERO_QUEUE` and `ZERO_PENDING` arrived with
`6c39b1b4` and this test with `8f74272d`, so the shape has been reachable since
the queue existed and no landing is a suspect.

So the sentence below is no longer the whole of it: this is a *semantic* event
riding a release the caller cannot wait for, which is what
`kernel/src/object/mod.rs`'s own header says must never happen — *"every
userland-visible lifecycle event rides `handle_count`"* — and the same shape
reaches soundd, whose cpal clients spend their lives parked in a signal-pipe
read.

## Why it matters beyond a test

Two harness binaries had to learn to settle before reading --- `handle_lifetime`
and `shm_release_reclaims`, whose verdicts are the per-kind object census either
side of the release. The consequence that is not a test is a process which kills
a child to make room and immediately allocates: the pages it just freed are not
free yet, and `SYS_SHM_CREATE`/`io_uring_setup` can answer `ResourceExhausted`
for memory the machine is in the middle of handing back. On a memory-tight
machine that is a spurious refusal, and nothing in the ABI lets the caller tell
it from a real one.

`ops::close_all`'s own doc states the intent this misses — *"Called by exit **and
by kill**, so the drops below are on the path a process taken down by another
CPU follows"* — which is about the drop happening, not about it having finished.

## What to do

Not "drain harder": every drain site already runs, and adding a fourth changes
nothing about a batch another CPU is holding. The two honest shapes are

- **Never publish a batch as absent while it is in flight.** Popping one object
  at a time and clearing `ZERO_PENDING` only when the queue is genuinely empty
  shrinks the window from "every object the kill queued" to "at most one per
  other CPU" — a mitigation, not a guarantee, and at four vCPUs three 2 MiB
  pages is still a visible amount.
- **Give the batch an owner.** The releasing syscall should run the hooks of the
  objects *it* retired, with nothing held, before it returns — which is what
  makes the kill path and the exit path one teardown rather than two, and what
  makes "a killed process holds nothing" a fact rather than a race.

The second is right and it is **not free-standing work**: it is the object
layer's release protocol, and no track owns it. What a hook released from this
queue is allowed to do is decided by three things the tree has: the proof a
park needs (`scheduler::Parkable`), a kill answered `Cancelled` at the park
(`kernel/src/watch.rs`) and the sleep lock (`kernel/src/sleeplock.rs`). The
constraint they leave, which `ZeroHandles::on_zero_handles`'s doc holds
(`kernel/src/object/mod.rs`) and which anything touching this queue must not
lose: **none of the three drain sites can park, so no `on_zero_handles` hook
may take a sleep lock at all.**

### What "give the batch an owner" costs, worked out 2026-08-20

A per-thread list cannot live behind `ThreadData`'s lock either:
`teardown_resources` holds `ProcessData` across `close_all`, and its own first
line is that the two locks are never held together. So the list has to be on the
kernel stack, threaded through `close_all` to a caller that runs the hooks once
its guard is gone.

**That is the part that is not a patch.** `HandleEntry`'s drop is where the
enqueue happens today, and the comment on that statement is the whole argument
for the design — *"this is the one statement that makes 'a hook cannot run under
a lock' structural"*. A `close_all` that hands its objects back for the caller
to run re-opens, for that one call site, precisely the guard-outlives-the-drop
trap the queue exists to make unwritable. Any owner-shaped fix has to pay for
that property somewhere else rather than spend it, which is why this is a
redesign of the release protocol and not a fix at the site.

`kernel-loom` is not the instrument for it, and that is worth writing down so
nobody re-derives it: the models compile the real kernel files with
`feature = "loom"`, and `object/mod.rs` pulls in `alloc::sync::Arc`, the whole
`kobject!` set and every subsystem those hooks reach. A transliteration is what
that crate's header exists to refuse.

That is the cost on the `deferred` rows. The one `immediate` row that had a
cost of its own was `File`, and the owner ruled on it.

## What the owner ruled on `File`'s release, 2026-08-23

`File` is not on this queue. `object/mod.rs` makes it an `immediate` row —
*"A file's flush and cache reference ride the last `Arc`"* — so its release is
`OpenFileState::drop` (`kernel/src/object/file.rs`), which runs under
`Lock<ProcessData>`. On 2026-08-20 that `Drop` took `vfs::lock()` and flushed
a modified file to the device, and the lock conversions then planned made
`vfs::VFS` a sleep lock, which no `Drop` could take: it cannot be handed a
`Parkable`, and moving `File` to `deferred` swapped one site that may not park
for another. The question went to the owner as "may a `Drop` impl park", in
three shapes:

1. **The write-back queue first.** A file's dirty pages outlive the handle
   that dirtied them until write-back reports complete, and `Drop` releases a
   cache reference and nothing else.
2. **Let this one `Drop` park, and say so.** `ops::close` and
   `teardown_resources` take the entry out and drop it once the `ProcessData`
   guard is gone, and "a handle to a `File` may only be dropped from a context
   that may park" becomes a rule nothing enforces.
3. **Give the batch an owner**, the second shape under "What to do", **and
   make `File` deferred**, which needs `drain_zero_handles`'s two scheduler
   sites to stop running hooks that can park: a redesign of this queue rather
   than a use of it.

The ruling, in the words it was recorded in, where "wall 4" is this question:

> **Owner ruling on wall 4, 2026-08-23: shape 1 — the write-back queue chunk
> lands first.** `vfs::VFS`'s conversion is sequenced behind the
> write-back-queue chunk […]. That chunk's invariant — a file's dirty pages
> outlive the handle that dirtied them until write-back reports complete —
> leaves `OpenFileState::drop` releasing only a cache reference, which a
> `Drop` may do, so no new rule ("a `Drop` may take a sleep lock") is created
> and `scheduler::Parkable`'s header sentence stays true as written. Shapes 2
> and 3 are declined precisely because they would create that rule or redesign
> the zero-handle drain; shape 1 is the one that needs neither.

The queue landed with #257 (`7ab9367b0`) and left the kernel in `80a1f1ceb`,
when nothing the kernel flushed reached a device any more; the conversion it
was sequenced before is planned by nothing. Today `OpenFileState::drop` calls
`file_cache::release` and takes "the file cache's lock and no other", so
`File` needs no release site that parks.

Open with the owner: he declined shape 3 because it would "redesign the
zero-handle drain", and whether that decline reaches "give the batch an owner"
for the `deferred` rows is not ruled.

## A fourth witness: a device claim, and init waiting it out

A service swap (`toyos-swap`, `/system/bin/init`) kills a service, `wait`s for
it, and mints the service's device claims again for the binary that replaces
it. `Device` is a `deferred` row, so the `wait` returning is not the claim
being back. Measured on `wt/toyos-swap`, `tests/swapcase` (virtio-net, two
CPUs, TCG), one run of four, three guests at once on the dev host:

```
init: swap netd: stopping: pid 5 (/system/bin/netd)
init: netd: pci:1af4:1041 is already claimed
[kernel 1.162 cpu0] exit: netd pid=5 code=137 cpu=117ms
[kernel 1.163 cpu1] pcidev: PCI 00:03.0 [1af4:1041] released from slot 0
```

The claim was asked for, and refused as held, before the release record — and
the release ran on the other CPU. The replacement netd started with no NIC and
exited, which init's probation caught and answered by restarting the old
binary five seconds later, when the claim was long back.

**The compromise, recorded here as the rule asks:** `userland/init`'s
`CLAIM_RETURN` retries a claim refused as `AlreadyExists` for a service init
has just stopped, for at most two seconds, one millisecond apart. Owner: the
swap's author. Exit condition: this issue closes — the kernel publishes a
process's end only once its deferred releases have run — and `CLAIM_RETURN`
is deleted with it.

**A second loop waits the same release out.**
`tests/toyos-rust-tests/src/isa_row.rs`'s `claim_after` asks for the `isa` row
again while the claim is refused as `AlreadyExists`, for at most five seconds,
wherever `isa_grant` or `isa_lines` claims the row after a holder has exited.
No run was red without it: it rests on the `Device` release measured in this
section. Owner: the `isa` claim's author. Exit condition: the same, and
`claim_after` is deleted with `CLAIM_RETURN` — once the release is no longer
deferred, a claim refused after its holder's exit is a defect that loop would
hide for five seconds.

**A third loop waits the same release out.**
`tests/toyos-rust-tests/src/bin/usbd_spare.rs` asks for the xHCI controller's
claim again after killing usbd while it is refused as `AlreadyExists`, for at
most `CLAIM_RETURN`, one millisecond apart, `userland/supervisor`'s copied. Owner:
usbd's author. Exit condition: the same, and that loop is deleted with
`CLAIM_RETURN`.

**Its tests are deleted**: `38a5064b6` took `handle_basic`, `handle_transfer`
and `kill_while_blocked` out, and `009db6db3` retired `SYS_DEBUG` actions 17 and
18, which only `handle_transfer` read. `02c35a85d` then moved `FILL`, which
`009db6db3` stopped writing over the idle stacks, into x86-64's `percpu`, so
`git revert 02c35a85d 009db6db3 38a5064b6` brings them back;
`git show 84471bc58:tests/toyos-rust-tests/src/bin/handle_transfer.rs` holds #536's
adaptation of the one #536 changed.
