---
status: open
kind: track
opened: 2026-10-28
---

# Anonymous memory swaps to an encrypted partition

**To build:** committed anonymous memory that may leave RAM for an encrypted,
authenticated swap partition and come back intact, with a userland memory
service choosing what leaves. Commitment counts RAM plus reserved swap. A
successful commitment never fails at a fault and never reads invented contents.
Swap adds capacity, not responsiveness.

Here "swap" means paging to the swap partition. `system/swap`, `lib/swap`
(`toyos-swap`) and the supervisor's swap port replace a running binary, which is a
different thing.

The design is out of the tree with the memory and storage designs. Its plans and
reviews are in #1088's evidence. Below is what an implementer needs.

## What a program sees

- **Swapped memory may wait on storage**, in a load, a store or a syscall that
  copies it.
  - A wait advances on storage progress, ends on established failure or kill,
    and is reported overdue.
  - A timeout never turns it into an error. `O_NONBLOCK` does not cover the
    caller's own buffer.
- **A page is lost only when a running memory service establishes the loss**: a
  read failed after the driver's recovery, a failed authentication, or a device
  that is gone.
  - With no service running, nothing is established, and waits stay overdue.
  - **The process's own anonymous memory**, once lost, ends the process at its
    next touch: a load, a store, or one of its own syscalls copying it. The
    syscall gets no errno.
  - **A buffer inside a file mapping** (a paged object, or a served `/tmp`
    object) answers `EIO`.
  - **A futex wait** on a swapped or lost word returns as a spurious wake.
- **Executable and read-only image pages never go to swap.**
- **A stopped memory service, or one past its restart budget, fails nothing.**
  Its faults wait, reported as "memory service not running", until an authorized
  manual restart. The user may stop any waiting program.
- **Commitment can outlive its backing.** If the swap device leaves, or
  retirements exhaust the spares, new commitments are refused. A fault past the
  remaining backing waits, reported with the deficit, and is never ended.
- **`mlock`/`vm_wire` is refused past the job's wire allowance** (`ENOMEM`).
- **One program's swapped memory delays only that program.** No service waits on
  one client's memory in a path that serves others.

## Ownership

- **Kernel:** mechanics only, with no policy, filesystem or cryptography.
  - Faults, their waits and the overdue reports.
  - Accessed and dirty harvesting, and reverse records.
  - The rules in Accounting.
  - Revalidation, unmap, shootdown, futex revoke and free.
  - The mechanical no-I/O fallback.
  - **The backing object, one per boot.** It holds each block's state, claim
    number and tag, the 256-bit key from the kernel RNG, and the claim counter.
    - No process holds a handle to it, and only the boot's end destroys it.
    - Every `backing_*` call requires the `reclaimer` role.
    - `backing_attach` hands the key and the next claim number to the caller,
      and is refused while another live process is attached.
- **Memory service** (new; a signed `system.toml` row with the `reclaimer` role):
  - watermarks and its reserve `M`;
  - reclaim selection over file and anonymous pages;
  - swap-out and swap-in, block placement, sealing and opening;
  - pressure figures.

  It keeps nothing a restart needs, and reads only pages it has begun.
- **File servers:** dirty file pages are written to their file, never to swap.
  Clean ones are dropped.
- **The storage driver** carries the swap session's I/O. diskserver opens and
  reopens sessions.
- **The supervisor:**
  - the `reclaimer` and `resident` spawn attributes, from signed rows only,
    refused in both orders;
  - restart holds;
  - wire allowances, as the permission service decides them;
  - automatic and manual restarts.

The lock order is object, then space.

## Accounting

```text
 resident rule:  Σ_j max(F_j, W_j)        + K + M ≤ R
 backing rule:   Σ_j max(F_j, W_j + A_j)  + K + M ≤ R + S
 frame rule:     a W promise is a held frame, never taken by a swappable fault.
                 An A promise is a backing unit and holds no frame.
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
- **Residency follows the job an object is charged to**: its creator, or the job
  named with `SHM_FOR_JOB`.
  - Everything charged to a resident job is `W`.
  - **Resident by kind**, whatever the job:
    - kernel-written objects (inbox rings, TLS blocks, the clock page);
    - anything lent to a device, where lending moves the charge from `A` to `W`;
    - sealed immutable versions;
    - boot payloads;
    - a pager's core (`kernel/src/object/paged.rs`): its dirty ceiling, the
      page-in headroom a read's supply can always draw, its index and
      notices. Its clean pages above that are the memory service's slack
      (`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`).
  - Everything else is `A`.
- **A swappable fault** takes an unpromised frame. Failing that, it takes the
  mechanical fallback: a clean, unpinned slack page, preferring the service's
  candidates, and never a `W` frame. Only then does it wait for reclaim. With
  `S = 0`, nothing depends on the service.
- **Superframes under pressure** are freed by targeted evacuation (drop and
  swap-out), never by migration.
  - Unmovable 4 KiB frames come from their own superframes.
  - A request that can wait waits. Otherwise it is refused `ResourceExhausted`
    and reported.
  - Only heap growth that cannot wait takes a kept superframe, and the next
    evacuation refills `K` first.
- **The ledger** splits committed into resident and swappable. It adds swapped,
  refaults, stall time, wired total, wire allowance and wire refusals to
  `SYS_ACCOUNT_INFO`.

## Constraints settled

- **One authoritative copy of each live anonymous page at every instant.**
  - **Claim before writing.** `clean_begin` claims the blocks it names
    atomically, and is refused whole if any is not free. A referenced block is
    never written.
  - **Free after durable.** `clean_end` comes only after a flush answers durable
    for every write of the batch. The block client keeps an acknowledged
    write's blocks, which the service hands the session shared, until a flush
    covers it, and reissues it from them after a loss
    (`lib/blockring/src/client.rs`); durable rests on that.
  - **Install before freeing.** Swap-in installs the frame and frees the block in
    one hold of the space lock.
  - **Re-dirtying.** A store, a kernel write copy, or a `mem_write` into a page
    being cleaned keeps it dirty, and keeps its frame.
- **Arena reserve.** A swap session's arena has 511 blocks and its ring has 64
  entries, and both carry only requests on the wire: an acknowledged write
  holds neither.
  - Writes on the wire hold at most 447 blocks and 48 entries, leaving 64
    blocks and 16 entries for swap-in reads.
  - What the session keeps shared is bounded by the batch: `clean_end` waits
    for a flush covering it. Past `diskserver::session::KEPT_BLOCKS` the
    session flushes for itself before the next write. The service's batch size
    is its own choice and needs no arena arithmetic.
  - **A swap-in read can wait behind a write.** Requests go out in the order
    asked, so a read asked after a write waits while that write has no
    contiguous arena room, or waits at the session's kept limit for its flush.
    The 64-block reserve bounds how many reads fit, not that wait. If swap-in
    latency matters, reads go out ahead of waiting writes, which the client's
    one outbox does not do today.
- **`CLEANING` on private leaves.**
  - A store faults and clears it. So does a kernel write copy, which takes its
    pin under the space lock. A read copy leaves it.
  - A page with a write pin is never begun.
  - `vm_remap`'s `move_leaves` carries swapped entries and `CLEANING` leaves, and
    rewrites the moved frames' reverse records.
- **Block records.** One 8-byte kernel record per block holds 3 state bits (free,
  claimed, referenced, failed, retired) and a 61-bit claim number, with a 16-byte
  tag beside it.
  - The claim number comes from one monotone counter and is both generation and
    nonce. `mem_supply` and `mem_fail` name it, and a stale one is refused.
  - A swapped leaf holds only the block number, encoded clear of cacheable
    physical addresses (L1TF).
  - v1 supplies only waiters, with no read-ahead. A wire or a `Pending` copy
    requests its whole range.
- **AEAD.**
  - Epoch e = c >> 26, sequence c mod 2^26, subkey HKDF(key, e), with no AAD.
  - That is 2^26 pages (256 GiB) per subkey, under RFC 8446 §5.5's AES-GCM limit.
    One epoch length serves ChaCha20-Poly1305 too.
  - AES-256-GCM where the CPU has AES and carry-less multiply, else
    ChaCha20-Poly1305. AArch64 uses ChaCha20-Poly1305 today, because `lib/crypto`
    has no AArch64 AES.
  - `toyos-crypto` gains `SealingKey::at(key, next)` and `seal_at(sequence, ..)`,
    monotone with gaps.
  - The service seals each claim at most once, by consuming the begin's `Claim`.
    A retry takes a new claim. A reissue resends the same bytes.
- **Restart.**
  - When the memory service ends, its begun cleanings fail (the pages stay
    resident and dirty) and their claims are freed.
  - A dead session holds its partition until each of its commands is answered
    (`lib/blockserve/src/sessions.rs` `place`, `retire`). A driver restart resets
    the controller before it serves (`system/nvme`).
  - The service's job and restart hold are kept as long as the backing is.
- **Retirement.** A block whose write fails after recovery is retired and replaced
  from the spare reserve.
- **Copied image segments.**
  - A copy range of a non-resident job that has been *never writable* since
    creation, with an immutable source, is never swapped. A page of it is
    dropped and copied again at its next touch, and stays charged in `A`.
  - *Never writable* is a range flag set at creation and cleared by any
    `vm_protect` to `ReadWrite`, so RELRO and `.data` do not qualify.
  - A copy range counts as a mapping of its source, for release and purge.
- **Sealed versions and boot payloads stay resident.**
- **The TOYOS-SWAP partition** is a GPT type on internal storage only, granted by
  type to the memory service. With no partition `S = 0`, logged by name. Never a
  swap file.

### Services and client memory

A service's shared path touches client memory only through calls that never wait.

- **`mem_read`/`mem_write`** on a RAM object answer `Done`, `Pending(ticket)`,
  `Busy` or `Lost(cause)`, through a ticket table the caller commits.
  - They require `READ` and `WRITE` respectively.
  - `Pending` requests every non-resident page and raises `available`.
  - A `mem_read`'s loads are ordered before the caller's later accesses:
    `DMB ISHLD` on AArch64, TSO on x86-64.
- **`vm_wire_ticket`** of an object charged to another job answers `Done`,
  `Pending(ticket)` or `Lost(cause)`.
  - It adds the wirer's own `W` and allowance charge at the call, where it may be
    refused. The owner keeps its `A`.
  - An unmap or `vm_unwire` cancels it and removes only the wirer's charge.
  - A blocking `vm_wire` of another job's range is refused `InvalidArgument`.
- **After `Lost`** on a client's memory, the service ends that client's session
  with its protocol's ordinary failure. No service answers `EIO` for a client's
  own memory.
- **Swap-chain links** neither map a paged object, nor wait on a file server, nor
  touch client memory except through these calls. None allocates per I/O.

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
  - **A refused restore is recoverable exhaustion.** The app and its state stay
    intact, the shortage is explained, and the controls stay usable. The restore
    can be retried. It names its cause: either the compositor's allowance is
    spent, or resident charges machine-wide have reached `R − K − M`.
- **netstack** copies stream data spans, and wires each region's 4 KiB header.
  - `toyos-net-tcp`'s `Lent::put`/`take` (`system/netstack/shard/tcp/src/ring.rs`)
    become fallible.
  - A placement answering `Pending` is not acknowledged.
- **The file server** copies requests staged in each client's window.
- **logkeeper** wires each ring's header. It reads a slot's `published` word and
  then its body, in two ordered `mem_read`s.
- **Window buffers** are created with `SHM_FOR_JOB` for the client.
  - That needs a new job right, `CHARGE`. `SHM_FOR_JOB` requires it, no spawn,
    floor or allowance path accepts it, and the rights the kernel mints to a job
    include it.
  - A client grants `CHARGE | TRANSFER`. The compositor keeps `CHARGE` alone.

### Resident rows

**Whole-process `resident` rows** are signed only, and their images come from
objects the supervisor holds:
- the supervisor, the input servers, usbd and the compositor;
- soundserver and the audio drivers;
- the memory service, the storage driver and diskserver;
- the file servers, for their pagers' cores alone: a file's pages are backed by
  the file, so a dirty one is written back by its pager and never swapped.

Each row states a `restart-hold` in whole superframes. The hold is kept while the
process is dead, counted as that job's `W`, and released only to its next start.

**A swap-in needs:**
- the waiter record;
- the page tables holding the entry;
- the block records (24 B per block);
- reverse records (8 B per frame);
- the resident chain;
- the session's two doorbell events;
- `M`.

**Wiring.** Each job has a wire allowance (Sizes).
- Larger allowances are decided by the permission service and applied by the
  supervisor, beside `SYS_JOB_FLOOR`.
- Helpers share their job's allowance.

### Sizes, provisional

P5 adjusts each one against the bench workloads' ledgers.

- **Wire default: 8 MiB.** It rises if ordinary apps meet refusals, and falls if
  ambient wiring nears the resident rule's headroom.
- **Swap: `clamp(RAM/2, 4 GiB, 16 GiB)`, set at install.** It is too small if
  swapped memory stays near `S` with low refaults, and too large if it never
  fills a small fraction. Resizing means repartitioning.
- **Spare reserve: 0.1 % of the partition, not counted in `S`.** It grows if a
  bench retires half of it, and a drive retiring at that rate is reported as
  failing.

## Prerequisites

No in-tree track yet covers two of these, and their owners file it.
- **Sealed spawn sources:** a spawn from a writable image object is refused.
  - There is no in-tree track yet. The owner of
    `issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md` files it.
- **The paged-object interface and coherent shared file mappings:** item 13 of
  `issues/toyos-builds-itself.md`. Its first part, paged objects read through
  `SYS_PAGED_READ` and no mapping, is built (memory stage 7a,
  `kernel/src/object/paged.rs`); mappings are stage 7b
  (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).
- **The file server's data cache deleted**: done with 7a. A file's pages are its
  paged object's (`system/fileserver/src/pages.rs`), and
  `system/fileserver/src/cache.rs` keeps the volumes' structures alone.

## Stages

Each check goes red under the mutation named after "red".

| Stage | After | Builds | Exit |
|---|---|---|---|
| S0-a (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`'s owner) | — | Every user copy faults or swaps in its chunk before any kernel lock, and pins after a re-check, with no pin across a wait. A lost anonymous page ends the caller, and a lost file-mapping page answers `EIO`. The futex re-translation never waits | Closes `issues/a-user-copy-demand-pages-under-whatever-its-caller-holds.md` and `issues/a-pipes-lock-spans-a-user-copy-and-its-first-allocation.md`. **H:** a chunk model in which an anonymous loss ends the caller with no partial result and a file-mapping loss answers `EIO`, red if either takes the other's outcome. **Q:** the pipe and futex sites, red when a copy runs under the pipe lock |
| S0-b (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`'s owner) | — | The block-session client and driver side allocate nothing per I/O: tables sized by `DEPTH`, reads opened straight from the arena. A reopen refused `Held` by a closing session waits for its retirement | **H:** `DEPTH` traffic with an allocator that fails after warm-up, red on any per-I/O allocation; a reopen during a dead session's close answered only after its last completion, red if the hold is released early |
| P1 (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`'s owner) | prerequisites | Both rules and the frame rule in `kernel/pure/frames.rs`; residency by charged job; `CHARGE`; `vm_wire`, the per-job allowance and additive charges; `resident` and `restart-hold`; the main stack sized by `PT_GNU_STACK` (8 MiB otherwise); resident by kind; unmovable frames in their own superframes; the ledger figures; `S = 0` | **H:** both rules at every `W` and `A` increase, red when a table build skips the backing rule; a resident fault never waits, red when a swappable fault may take a held frame; `SHM_FOR_JOB` to a resident job gives `W` and to an ordinary one `A`; wiring refused past the allowance; a helper shares its job's allowance; an additive wire, red if the charged job pays; two wirers where the first exits, red if any release can be refused or leaves a rule exceeded; superframe invariants under wire and lend; a hold kept whole. **Q:** a `CHARGE`-only handle creates an object `SHM_FOR_JOB` and is refused as a spawn's job and at `SYS_JOB_FLOOR`, red if either refusal accepts it; `mmap_stress`; each resident row's committed and populated totals reported |
| P2 (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`'s owner) | P1 | Reverse records and the RAM-object reverse index; `mem_scan`; `mem_reclaim` for clean pages and never-writable copied pages; `mem_read`/`mem_write` and `vm_wire_ticket` on the ticket table; watermarks; the kept fallback; superframe evacuation; the `reclaimer` role; service v0 (plain CLOCK) | **H:** revalidation, floor protection, revoke order, an evacuation case; `Pending` without waiting, every non-resident page requested at once, red if a call waits or requests fewer; an unmap cancels a parked ticket and removes the wirer's charge without waiting, red if the unmap waits or the charge stays; `Lost` removes the wirer's charge, red if it stays; a blocking wire of another job refused, red if accepted; a dropped copied page refilled byte-exact, red if a once-`ReadWrite` or resident range is dropped; a version not purged under a copy range, red if the range is not counted; the fallback with a stuck service; a remap between scan and reclaim. **Q:** a page-cache-heavy run with the controls usable |
| C-wire (`issues/toyos-has-a-desktop.md`'s owner) | P1, P2 | Non-blocking wires of on-screen buffers from the loop; shown only once wired; the refusal points; window buffers charged to clients through `CHARGE`; the loop's file I/O (the launcher's app list, the kept scale) moved off it | **H:** red if the loop composes an unwired buffer; a resize keeps the old buffer until wired, red if it swaps early; one parked wire delays no other window; after `Lost` the compositor keeps running and drops that window, red if it ends; a close during a parked wire unmaps without waiting; the kept handle `CHARGE` alone, red if it carries `TRANSFER` or `DUP`. **Q:** with one buffer parked unresident, and with a parked file call, the other windows draw and the stop control works, red when the file call is made on the loop |
| S-copy (owners of `issues/toyos-has-its-own-network-stack.md`, `issues/storage-is-layers-and-a-role-is-a-filesystem.md` and `issues/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`) | P1, P2 | netstack's spans, the file server's client windows and logkeeper's slots through `mem_read`/`mem_write`; header pages wired; fallible `Lent::put`/`take` | **H:** a `Pending` client deferred while the others are served, red if the shared path waits; a header wire `Pending` on a restart (logkeeper re-wiring every ring) delays no other ring; a client whose copy answers `Lost` ended while the service serves on; logkeeper's torn record, red when word and body are read in one copy |
| P3 | prerequisites, S0-a, S0-b, P1, P2, C-wire, S-copy | Block states and claims; the spare reserve; the backing and `backing_add`/`retire`/`query`/`attach`; anonymous cleaning ended after durable; TOYOS-SWAP; `at`/`seal_at`; device-gone failure; the stopped-service and deficit reports; memory-format files as resizable anonymous objects with a *served* mark; C3's probe ignoring TOYOS-SWAP | **H:** `kept` exactness for private leaves and for `mem_write` into shared objects, red if `clean_end` frees a page `mem_write` wrote; claims never repeat and are refused on referenced blocks; a stale supply or fail refused after reuse; a frame never freed before durable, red when `clean_end` follows the acknowledgement; a nonce oracle across retries, reissues, kills, restarts and epochs, red when a claim is sealed twice; a retirement takes a spare and the deficit is reported past the spares; a spent budget leaves every record, entry, job and hold intact, red if any is failed or released; a device leaving with no service running leaves waits overdue, not failed. **Q** (q35, `virt`, small `-m`): commit beyond RAM, contents verified; a `blkdebug` read error and a ciphertext byte flipped through `qemu-io` each end only the owner; a lost `/tmp` page answers `EIO` while its server serves on; a `write()` from a lost anonymous page ends the caller; a driver reset and a service kill mid-batch lose nothing; a restart keeps data; a service stopped past its budget leaves waiters overdue, and a manual restart then serves them with contents intact; `/tmp` swapped and read back through `read()` and a mapping, the server never blocking; killing `/tmp`'s server ends a mapper with the invalid-mapping diagnostic |
| P4 | P3 | Policy: CLOCK-Pro and generational on the same scan signals; file and anonymous balance by refault cost; write-ahead if refaults justify it | **H:** replays of recorded scan streams, and synthetic loops, scans and Zipf. Lower refaults at equal cost wins, else the simpler |
| P5 | P3 | T14 and AMD bench | Swap throughput; flush cost on DATA; fault latency; a returning window's delay; netstack's rate with copies; stop-control latency under thrash; audio without underruns; the provisional sizes adjusted |

## What it supersedes in the tree

Each of these changes at the stage named:
- **P3: `issues/storage-is-layers-and-a-role-is-a-filesystem.md`.**
  - Its partition table, and "the installed product carries all four on one
    disk": a fifth, TOYOS-SWAP, joins them.
  - Its Volume section's type-GUID probe, which ignores TOYOS-SWAP.
  - "`/tmp` has no backing": `/tmp` files become anonymous objects whose pages
    may swap.
- **P1:** `kernel/pure/frames.rs`'s header ("A promise is a frame no other
  allocation may take"), and stage 4 of
  `issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md` ("A
  commitment is promised its frames when it is made"). An `A` commitment is a
  backing unit and holds no frame.
- **P2, P3:** `kernel/src/object/shm.rs`'s "Frames are the object's from its
  creation to its end" no longer holds for RAM objects, whose pages can swap.
- **P3:** `sdk/toyos/src/log/stdio.rs`'s "never waits" holds only for resident and
  wired rings.
- **C-wire:** `system/compositor/desktop/src/budget.rs`'s `WINDOW_BUDGET_SHARE`
  (the RAM/8 window cap) becomes the compositor's wire allowance over on-screen
  buffers.
- **Deleted.** `system/fileserver/src/cache.rs` goes at its prerequisite.
  `kernel/src/file_cache.rs` behind kernel tmpfs goes at P3's memory format.
- **When text maps its source without the copy**
  (`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`
  stage 5), the copied-segment rule goes with the copies.

## Known costs

P1 and P5 measure these on the current tree and the T14. A figure comes back only
if it shows a material limitation under normal workloads, or a measured
regression.

- **Resident rows:** ≈ 120 MiB with declared stacks. This is an estimate, derived
  from measured `committed + kernel` of `c06cc695e` on a 4 GiB QEMU guest.
  Touched sizes are unmeasured.
- **On-screen window buffers:**
  - ≈ 8.8 MiB per full-screen 1920×1200 window, by arithmetic;
  - up to ≈ 6.6 MiB per terminal window, a measured upper bound (the
    compositor's whole climb per window).
- **Kernel tables:**
  - block records are 0.59 % of `S`, by arithmetic;
  - reverse records are 0.2 % of `R`, by arithmetic on an estimated 8-byte
    packing.
- **A returning window's swap-in:** 10–30 ms on NVMe, an estimate.
- **Copy-shaped services:** one syscall per copied span, 0.1–0.5 µs each, an
  estimate.
