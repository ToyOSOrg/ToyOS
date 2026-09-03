---
status: open
kind: defect
opened: 2026-08-18
---

# The sites that still carry a boundary-crossing number in a plain integer

`toyos-untrusted`'s `Untrusted<T>` exists and the virtqueue used ring uses it:
`Virtqueue::used_ring_id` and `::used_ring_len` (`kernel/src/drivers/virtio.rs`)
are the only reads of a used ring in the kernel and hand back `Untrusted<u32>`,
so `Virtqueue::parse_used` is the only path to an index or a length and it names
its bound at both. The type has no arithmetic, no `Deref`, no `From`, no cast
and no accessor; the four `compile_fail` doctests on `Untrusted` are what that
sentence means, and `src/sourcegate.rs` bans the one shape typing cannot stop
(`at_most(u64::MAX)` and its four siblings).

**The rest of the class has not adopted it.** Until a site does, its bound is a
thing an author has to remember rather than a thing the compiler asks for. Each
of these is mechanical — find the read, wrap it there, name the bound at the use
— and none needs a design decision. **Each is cited by file and enclosing
symbol**, so whoever takes this does not have to re-derive which value is meant.

## The sites, hardest consequence first

- **`kernel/src/drivers/nvme.rs`, the completion queue — converted.**
  `wait_completion` now takes the `cid` this driver put in the command
  (`(cmd.cdw0 >> 16) as u16`, read back out of the command it just submitted,
  not trusted again) and answers `Result<u16, Refused>`:
  `Untrusted::new(cq.cid).exactly(expected)`. `submit_and_wait` and all four
  callers (`admin`, `identify_namespace` through it, `read_sectors`,
  `write_sectors`) now match on the result and log-and-fail exactly as they
  already did for a bad status word. `Untrusted::exactly`'s own behaviour is
  covered by `toyos-untrusted`'s host test suite
  (`exactly_refuses_by_naming_both_numbers`).
  **No dedicated boot-level gate** — unlike the virtqueue used ring, this
  queue has no actuator today, and building one (`#[cfg(feature =
  "boot-actuators")]` writer + self-test + `actuator.rs` entry +
  `MACHINE_TESTS` registration) means a `tests/test-durations` entry, which
  this session watched block PR #118's own merge on exactly that mechanism
  (`ci/durations`: "committed UNMEASURED profile marker(s) are provisional and
  may not land"). Given the issue's own framing — "sound today only because
  every submission is synchronous, a property of the caller" — this is
  hardening against a future asynchronous queue rather than a live defect, so
  the landing risk was judged not worth it this pass. An
  actuator + self-test mirroring `drivers/virtio.rs`'s `used_selftest` is the
  future work if the owner wants it exercised on every boot.

## What this type does not answer

Recorded so nobody tries to make it. One entry in this area was **not** this
class, and wrapping something would have touched it:

- **netd trusting the ring's closed flags** — a *predicate* a peer writes, not
  an index. `RingHeader::flags` lives in the page `SYS_PIPE_MAP` maps writable,
  and netd read `is_reader_closed`/`is_writer_closed` as facts about its peer.
  There was no bound to compare a bit against, so the answer was not a wrapper
  but to stop reading a publication as a channel: netd now asks the kernel's own
  `readers`/`writers` counts, which surface as EOF on a read and `BrokenPipe` on
  a write. Closed on its own file.

A second entry used to stand beside it: virtio-net registering its whole
`DmaPool` page, so that the claimant mapped the descriptor tables writable. That
was a *mapping* rather than a value, no read-side type helped while it stood,
and it was closed on 2026-08-23 by splitting the pool — which is why this
section names one entry and not two.

## What this type is not about

It carries no *time* and no *rate*, and nothing in it reasons about how long
anything takes. The TCG-versus-KVM p90 divergence PR #119 measured (4–8× in p90,
20× in max for scheduler pass cost on the dev host) does not reach any bound
here: every one is a byte count, a table length or a register encoding, and each
is compared against a number the driver itself published in the same function.
Recorded so that nobody re-checks it.
