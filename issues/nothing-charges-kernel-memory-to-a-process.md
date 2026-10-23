---
status: open
kind: track
opened: 2026-08-04
---

# Nothing charges a kernel allocation to the process that caused it

**Kernel memory a process causes is charged, but to the machine, not to
it.** Every persistent kernel object a process can reach holds a
`mm::charge::Charge` for the heap it keeps, taken before the allocation and
refused `ResourceExhausted` at the call where covering it would take what the
frame allocator holds back for the kernel; the heap grows into what charges
reserved (`kernel/pure/frames.rs`). So the terminal state below is gone for
those objects. What is still missing is the attribution: no charge names the
process or job that caused it, so there is no per-process memory in `ps`, no
`meminfo`, no reap-time assertion that a dead process's charges went to zero,
and no way for the suite to assert the absence of a leak. The memory design
puts this on the scheduler's job as its `Account`, with `account_info`.

**Exit:** each charge held against its job's account, released with the
object, and `account_info` reporting it, with a reap-time check that an ended
job's charges are what its retained objects hold.

**The security case that once justified this is mostly gone, and that is the
news.** It was staged as an ownership discipline — a purpose-carrying allocator
type, aliases per subsystem, a charge on the four physical-memory chokepoints —
against a table of bugs that a different mechanism has since closed. Handle and
object lifetimes are refcount-driven with real `Drop` impls, so "a handle that
is not a value with a destructor" is unrepresentable; the descriptor type and
its un-refcounted clone are gone; the file cache installs a real budget and a
budget that was never installed is now a loud kernel bug; and the unbounded
user-string copy has a ceiling. What survives is the *accounting*, which nothing
has touched, plus one item: peak memory is written by two paths that overwrite
each other.

Accounting is a pillar of `issues/toyos-explains-itself.md`.

Blocked on nothing. Two things worth knowing before it is restarted, because
both cost a day to discover:

- **Omitting `#[global_allocator]` enforces nothing** — rustc errors whenever
  `alloc` is in the graph at all. The enforcement is a type alias with no
  default parameter, about 50 lines, not the Rust-for-Linux approach of dropping
  `alloc` entirely.
- **`#[must_use]` is inadequate for an obligation like this**: binding, `let _
  =`, `drop()` and burial in a collection all pass silently. A drop bomb is the
  state of the art, and `Unmapped<T>` is already exactly such an obligation.

**The terminal state of an uncharged grower is a halted machine.** A failed
kernel allocation panics with the size, the layer and the call site named, and
every kernel panic halts. What a call allocates and frees before it returns is
not charged: it draws on the superframes the kernel keeps, 1/256 of memory and
16 MiB at least, which nothing a process causes takes.
