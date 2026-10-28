---
status: open
kind: track
opened: 2026-10-28
---

# Anonymous memory swaps to an encrypted partition

**To build:** committed anonymous memory that may leave RAM for an encrypted,
authenticated swap partition and come back intact, with a userland memory
service choosing what leaves. Commitment counts RAM plus reserved swap and
nothing hoped for, so a successful commitment still never fails at a fault and
never reads invented contents. Swap adds capacity, not responsiveness. The design
is out of the tree with the memory and storage designs; the decisions an
implementer needs are below.

## What a program sees

- **Swapped memory may wait on storage**: in a load, a store, and in a syscall
  that copies it. A wait advances on storage progress, ends on established
  failure or kill, and is reported overdue. A timeout never turns it into an
  error, and `O_NONBLOCK` does not cover the caller's own buffer.
- **A page is lost only when loss is established**: a read that fails after the
  driver's recovery, a failed authentication, or the device leaving.
  - The process's own anonymous memory, once lost, ends it at its next touch:
    a load, a store, or one of its own syscalls copying it. A syscall gets no
    errno, because none means "your own memory is lost", and an errno would be
    read as a failure of the descriptor.
  - A buffer inside a file mapping (a paged object, or a served `/tmp` object)
    keeps the file contract: the syscall answers `EIO`.
  - A futex wait on a swapped or lost word returns as a spurious wake.
- **Executable and read-only image pages never go to swap.**
- **A stopped memory service, or one past its restart budget, fails nothing.**
  Its faults wait, reported overdue as "memory service not running", until an
  authorized manual restart. The user may stop any waiting program.
- **Commitment can outlive its backing.** If the swap device leaves, or
  retirements exhaust the spare reserve, new commitments are refused. A fault
  past the remaining backing waits, reported overdue with the deficit, and is
  never ended.
- **`mlock`/`vm_wire` is refused past the job's wire allowance** (`ENOMEM`).
- **One program's swapped memory delays only that program.** A service that
  serves many programs never waits on one program's memory in its shared path.

## Ownership

- **Kernel:** the mechanics, with no policy, filesystem or cryptography.
  - Faults, their waits and the overdue reports.
  - Accessed and dirty harvesting, and reverse records.
  - Both commitment rules and the frame rule.
  - Revalidation, unmap, shootdown, futex revoke and free.
  - The mechanical no-I/O fallback.
  - **The backing object, one per boot.** It holds each block's state, claim
    number and tag, the swap key, and the claim counter. No process holds a
    handle to it, and only the boot's end destroys it.
- **Memory service** (new; a signed `system.toml` row with the `reclaimer`
  role): watermarks and its reserve `M`; reclaim selection over file and
  anonymous pages; swap-out and swap-in; block placement; sealing and opening;
  pressure figures. It keeps nothing a restart needs.
- **File servers** keep file pages: dirty pages are written to their file,
  never to swap, and clean ones are dropped.
- **The storage driver** carries the swap session's I/O. diskserver only opens
  and reopens sessions.
- **The supervisor** owns:
  - the `reclaimer` and `resident` spawn attributes, from signed rows only;
  - restart holds;
  - wire allowances, which the permission service decides and the supervisor
    applies;
  - restarts, automatic and manual.

## Accounting

```text
 resident rule:  Σ_j max(F_j, W_j)        + K + M ≤ R
 backing rule:   Σ_j max(F_j, W_j + A_j)  + K + M ≤ R + S
 frame rule:     a W promise is a promise as kernel::frames keeps it today, a frame held back,
                 never taken by a swappable fault. An A promise is a backing unit and holds no frame.
```

The symbols:
- `R`: frames managed;
- `K`: kept superframes;
- `F_j`: job *j*'s floor, which also protects `min(F_j, use_j)` from reclaim;
- `W_j`: its resident charges;
- `A_j`: its swappable commitment;
- `S`: swap blocks, less the spare reserve and the retirements past it;
- `M`: the reclaimer's reserve, at most one superframe.

- **Every `W` increase checks both rules**: page tables, heap charges, runs,
  wiring, floors, lending and `vm_remap` growth. Every `A` increase checks the
  backing rule.
- **Residency follows the job an object is charged to.** That is its creator, or
  the job named with `SHM_FOR_JOB`.
  - Everything charged to a resident job is `W`.
  - **Resident by kind**, whatever the job:
    - kernel-written objects (inbox rings, TLS blocks, the clock page);
    - anything lent to a device;
    - sealed immutable versions;
    - boot payloads.
  - Everything else is `A`.
- **A swappable fault** takes an unpromised frame. Failing that, it takes the
  mechanical fallback (a clean, unpinned slack page, preferring the service's
  candidates; never a `W` frame). Only then does it wait for reclaim. With
  `S = 0` nothing depends on the service.
- **Superframes under pressure:** targeted evacuation by drop and swap-out, never
  migration. Unmovable 4 KiB frames are drawn from their own superframes. A
  request that cannot wait takes a kept superframe, and the next evacuation
  refills `K` first.
- **The ledger** splits committed into resident and swappable, and adds swapped,
  refaults, stall time, wired total, wire allowance and wire refusals to
  `SYS_ACCOUNT_INFO`.

## Constraints settled

- **The last recoverable copy.** Each live anonymous page has one authoritative
  copy at every instant.
  - **Blocks are claimed at `clean_begin`, before any write.** The claim is
    atomic, and the call is refused whole if any named block is not free. A
    referenced block is never written.
  - **`clean_end` comes only after a flush answers durable** for every write of
    the batch. Until then the frame is the authoritative copy. The block client
    keeps acknowledged writes and reissues them after a loss
    (`lib/blockring/src/client.rs`), and durable rests on that retention.
  - **Swap-in installs the frame and frees the block** in one hold of the space
    lock.
  - **A page stays dirty if anything wrote it during its cleaning:** a store, a
    kernel write copy, or a `mem_write`. Its frame then stays.
- **Cleaning private leaves.** A private leaf being cleaned carries the software
  bit `CLEANING`.
  - A store faults and clears it. So does a kernel write copy, which takes its
    pin under the space lock. A read copy leaves it.
  - A page with a write pin is never begun.
  - `vm_remap`'s `move_leaves` carries swapped entries and `CLEANING` leaves, and
    rewrites the moved frames' reverse records.
- **Block records.** One 8-byte kernel record per block holds 3 state bits
  (free, claimed, referenced, failed, retired) and a 61-bit claim number. A
  16-byte tag sits beside it.
  - The claim number comes from one monotone counter, and is both the generation
    and the nonce.
  - `mem_supply` and `mem_fail` name the block and its claim number, and a stale
    one is refused.
  - A swapped leaf holds only the block number, encoded clear of cacheable
    physical addresses (L1TF).
- **AEAD.**
  - The key is 256 bits, from the kernel RNG, made with the backing.
  - Nonce epoch e = c >> 26, sequence c mod 2^26, subkey HKDF(key, e). That is
    2^26 pages, 256 GiB, per subkey, inside RFC 8446 §5.5's AES-GCM margin. One
    epoch length serves ChaCha20-Poly1305 too.
  - There is no AAD, since a claim names one block.
  - AES-256-GCM where the CPU has AES and carry-less multiply, else
    ChaCha20-Poly1305. AArch64 uses ChaCha20-Poly1305 today, since
    `lib/crypto` has no AArch64 AES.
  - `toyos-crypto` gains `SealingKey::at(key, next)` and `seal_at(sequence, ..)`,
    monotone with gaps. The service seals each claim at most once, consuming the
    `Claim` the begin answered. A retry takes a new claim. A reissue resends the
    same bytes.
- **Restart.**
  - When the memory service ends, the kernel fails its begun cleanings (pages stay
    resident and dirty) and frees their claims.
  - A dead session holds its partition until each of its commands is answered
    (`lib/blockserve/src/sessions.rs` `place`, `retire`). So a restarted
    service's writes follow the dead session's completions. A driver restart
    resets the controller before it serves (`system/nvme`).
  - The service's job and restart hold are kept as long as the backing is.
- **Retirement.** A block whose write fails after recovery is retired and replaced
  from the spare reserve.
- **Copies of image segments.** A copied page whose range was never writable and
  whose source is immutable is never swapped. It is dropped, and copied again
  from its source at its next touch.
  - This applies only to non-resident jobs, and it is charged in `A`.
  - A copy range counts as a mapping of its source, for its release and purge.
  - The rule goes once text maps its source directly
    (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`
    stage 6).
- **Sealed versions and boot payloads stay resident.** Mapped code survives its
  file server's end.

### Services and client memory

A service's shared path touches client memory only through calls that never wait
on it. Both use a ticket table the caller commits.

- **`mem_read`/`mem_write`** on the client's object answer `Done`,
  `Pending(ticket)`, `Busy` or `Lost(cause)`.
  - `Pending` requests every non-resident page at once and raises `available`.
  - A `mem_read` orders its loads before the caller's later accesses: `DMB ISHLD`
    on AArch64, and TSO on x86-64.
- **`vm_wire_ticket`** of an object charged to another job answers `Done`,
  `Pending(ticket)` or `Lost(cause)`.
  - It adds the wirer's own `W` and allowance charge at the call, where it may be
    refused. The owner keeps its `A`.
  - `Pending` requests every non-resident page of the range at once.
  - An unmap or `vm_unwire` cancels the ticket and removes only the wirer's
    charge, so no release can fail a rule.
  - A blocking `vm_wire` of another job's range is refused `InvalidArgument`.
- **After `Lost` on a client's memory**, the service ends that client's session
  with its protocol's ordinary failure. The client ends at its own next touch. No
  service answers `EIO` for a client's own memory.

Who uses which:
- **The compositor** wires every on-screen window's buffer (visible or covered)
  from its loop, at its own charge, within a granted allowance of RAM/8. It
  unwires a minimized window.
  - A given buffer is shown, and a minimized window returns, only once its wire
    completes. Until then the old buffer stays shown.
  - Focus and hit-testing follow what is composed.
  - **Refusals:**
    - creation answers `REFUSED_NO_MEMORY`;
    - a resize keeps the old buffer;
    - a restore stays minimized.
  - **A refused restore is recoverable resource exhaustion.** The app and its
    state stay intact, and the window stays minimized with the shortage
    explained. The inspection and stop controls stay usable, and the restore can
    be retried once capacity returns. A refusal never kills work or freezes
    another client.
  - **It has two causes, reported as what they are:**
    - **the compositor's allowance is spent.** This is an ordinary case: minimize
      a window, open others, then restore it. Closing or minimizing another
      window makes room;
    - **resident charges machine-wide reach `R − K − M`.** This is physical
      exhaustion.
- **netstack** copies stream data spans, and wires each region's 4 KiB header.
  `toyos-net-tcp`'s `Lent::put`/`take` (`system/netstack/shard/tcp/src/ring.rs`)
  become fallible. A placement answering `Pending` is not acknowledged.
- **The file server** copies requests staged in each client's window.
- **logkeeper** wires each ring's header. It reads a slot's `published` word,
  then its body, in two ordered `mem_read`s.
- **Window buffers** are created with `SHM_FOR_JOB` for the client.
  - That needs a new job right, `CHARGE`. `SHM_FOR_JOB` requires it, and no
    spawn, floor or allowance path accepts it.
  - A client grants `CHARGE | TRANSFER`. The compositor keeps `CHARGE` alone and
    passes it nowhere.

### Resident rows

**Whole-process `resident` rows** (signed only; their images come from objects
the supervisor holds):
- the supervisor, the input servers, usbd and the compositor;
- soundserver and the audio drivers, so that the mix path never starves and
  audio quality is unchanged;
- the memory service, the storage driver and diskserver.

Each row states a `restart-hold` in whole superframes. The hold is kept while the
process is dead, counted as that job's `W`, and released only to its next start.

**What a swap-in needs:**
- the faulting thread's waiter record;
- the page tables holding the entry;
- the block records (24 B per 4 KiB block, 0.59 % of `S`);
- reverse records (8 B per frame);
- the resident chain: memory service, driver, diskserver, supervisor;
- the session's doorbell pipe;
- `M`.

**The chain must not allocate per I/O.** Swap-out needs no free frame.

**Wiring:**
- Each job has a wire allowance, 8 MiB by default.
  - Larger allowances are decided by the permission service and applied by the
    supervisor, beside `SYS_JOB_FLOOR`.
  - Helpers share their job's allowance.
- A wire of another job's memory adds the wirer's charge and never moves the
  owner's.

### Sizes, provisional

Each is adjusted against the bench workloads' ledgers in P5.

- **The wire default is 8 MiB.** It holds a realtime thread's stack and its audio
  periods. It rises if ordinary apps meet refusals, and falls if ambient wiring
  nears the resident rule's headroom.
- **Swap is `clamp(RAM/2, 4 GiB, 16 GiB)`, set at install**, on a dedicated
  internal TOYOS-SWAP GPT partition; never a swap file, never removable media.
  - It is too small if swapped memory stays near `S` with low refaults, and too
    large if it never fills a small fraction.
  - Resizing means repartitioning.
- **The spare reserve is 0.1 % of the partition, not counted in `S`.** It grows if
  a bench retires half of it, and a drive retiring at that rate is reported as
  failing.

## Stages

| Stage | Builds | Exit |
|---|---|---|
| S0-a (memory track) | Every user copy faults or swaps in its chunk before any kernel lock, and pins after a re-check, with no pin across a wait. A lost anonymous page ends the caller, and a lost file-mapping page answers `EIO`. The futex re-translation never waits | `issues/a-user-copy-demand-pages-under-whatever-its-caller-holds.md` and `issues/a-pipes-lock-spans-a-user-copy-and-its-first-allocation.md` closed. A host chunk model covers both loss branches. A guest mutation that copies under the pipe lock goes red |
| S0-b (storage track) | The block-session client and driver side allocate nothing per I/O, with tables sized by `DEPTH` and reads opened straight from the arena. A reopen refused `Held` by a closing session waits for its retirement. If pipes take frames as they fill, the session's doorbell pipe keeps its first one | A host test under `DEPTH` traffic with an allocator that fails after warm-up. A reopen during a dead session's close is answered only after its last completion |
| Prerequisites | Sealed spawn sources: no spawn from a writable image object. The paged-object interface and coherent shared file mappings (item 13 of `issues/toyos-builds-itself.md`). The file server without its data cache | As their own tracks state |
| P1 (memory track) | Both rules and the frame rule in `kernel/pure/frames.rs`; residency by charged job; the `CHARGE` right; `vm_wire` with the per-job allowance and additive charges; `resident` and `restart-hold`; the main stack sized by `PT_GNU_STACK` (8 MiB otherwise); resident by kind; unmovable frames in their own superframes; the ledger figures; `S = 0` | Host laws for both rules at every `W` and `A` increase, a resident fault that never waits, wiring past the allowance, an additive wire of another job's object, and two wirers where the first exits with no check at the release. A guest check that a `CHARGE`-only handle is refused as a spawn's job and at `SYS_JOB_FLOOR`. Each resident row's committed and populated totals reported |
| P2 (memory track) | Reverse records and the RAM-object reverse index; `mem_scan`; `mem_reclaim` for clean pages and never-writable copied pages; non-blocking `mem_read`/`mem_write` and `vm_wire_ticket` on the ticket table; watermarks; the kept fallback; superframe evacuation; the `reclaimer` role; service v0 (plain CLOCK) | Host cases: `Pending` without waiting, every non-resident page requested at once, an unmap cancelling a parked ticket, `Lost` removing the wirer's charge, a blocking wire of another job refused; a dropped copied page refilled byte-exact, and never a once-writable or resident one; a version not purged under a copy range; a remap between scan and reclaim. A guest page-cache-heavy run with the controls usable |
| C-wire (compositor track) | Non-blocking wires of on-screen buffers from the loop; shown only once wired; the refusal points above; window buffers charged to the client through `CHARGE`. **The loop's own file I/O moves off it** (the launcher's app list, the kept scale) | Host: the visibility model; a resize that keeps the old buffer until wired; one parked wire not delaying another window; `Lost` leaving the compositor running; the kept handle `CHARGE` alone. Guest: with one buffer parked unresident, and with a parked file call, the other windows draw and the stop control works |
| S-copy (netstack, storage and log tracks) | netstack's spans, the file server's client windows and logkeeper's slots through `mem_read`/`mem_write`; header pages wired | Host: a `Pending` client deferred while the others are served; a client whose copy answers `Lost` ended while the service serves on; logkeeper's torn-record case, red when word and body are read in one copy |
| P3 | Block states and claims, the spare reserve, the boot-lifetime backing and `backing_add`/`retire`/`query`/`attach`, anonymous cleaning ended after durable, TOYOS-SWAP, the crate's `at`/`seal_at`, device-gone failure, the stopped-service and deficit reports; memory-format files as resizable anonymous objects with a *served* mark. After S0-a, S0-b, C-wire, S-copy, P1 and P2 | Host: `kept` exactness for private leaves and for `mem_write` into shared objects; claims never repeat; a frame never freed before durable; a nonce oracle across retries, reissues, kills, restarts and epochs; a spent budget leaving every record, entry, job and hold intact. Guest (q35, `virt`, small `-m`): commit beyond RAM with contents verified; a `blkdebug` read error and a ciphertext byte flipped through `qemu-io` each end only the owner; a lost `/tmp` page answers `EIO`; a driver reset and a service kill mid-batch lose nothing; a manual restart past the budget serves waiters intact |
| P4 | Policy: CLOCK-Pro and generational on the same scan signals; file and anonymous balance by refault cost | Replays of recorded scan streams: lower refaults at equal cost wins, else the simpler |
| P5 | T14 and AMD bench | Swap throughput; flush cost on DATA; fault latency; a returning window's delay; netstack's rate with copies; stop-control latency under thrash; audio without underruns; the provisional sizes adjusted |

## What it changes elsewhere

These change at the stage that builds them:
- **`sdk/toyos/src/log/stdio.rs`'s "never waits"** holds only for resident and
  wired rings: an app's own ring write may wait on its own ring's swap-in (P3).
- **`kernel/src/object/shm.rs`'s "a fault on a mapping of one never allocates"**
  no longer holds for RAM objects, which can be swapped (P2, P3).
- **`system/compositor/desktop/src/budget.rs`'s RAM/8 window cap** becomes the
  compositor's wire allowance over on-screen buffers. Buffers are charged to
  their clients (C-wire).
- **Storage's rule that nothing is ever swapped** is replaced by encrypted
  swap. Secrets reach the disk only as ciphertext under a key that never leaves
  RAM.

## Known costs, estimated pending measurement

Every figure here is an estimate. P1 and P5 measure them on the current tree and
the T14. A figure is reported back only if it shows a material limitation under
normal workloads, or a measured regression.

- **Resident rows:** ≈ 120 MiB with declared stacks. This is derived from
  `committed + kernel` readings of `c06cc695e` on a 4 GiB QEMU guest. Touched
  sizes are unmeasured.
- **On-screen window buffers:** ≈ 8.8 MiB per full-screen 1920×1200 window, by
  arithmetic. Up to ≈ 6.6 MiB per terminal window, an upper bound from the
  compositor's total climb per window.
- **Kernel tables:** 0.59 % of `S` and 0.2 % of `R`, by arithmetic.
- **A returning window** waits for its swap-in, estimated at 10–30 ms on NVMe.
- **A restore** can be refused, by the compositor's allowance or by machine-wide
  resident exhaustion, as recoverable exhaustion.
- **Copy-shaped services** pay one syscall per copied span, estimated at
  0.1–0.5 µs each.
