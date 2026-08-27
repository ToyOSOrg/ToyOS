---
status: open
kind: defect
opened: 2026-08-20
---

# `Ring::read`/`Ring::write` build `&[u8]`/`&mut [u8]` over a page the peer can write too

Writing `undocumented_unsafe_blocks`' required safety comments on
`toyos-abi/src/ring.rs` surfaced a question the module's own doc comment
doesn't quite answer. The header names the trust boundary precisely: `Ring`
(kernel-side: base pointer, capacity, both cursors) lives in kernel memory,
`RingHeader` is what `SYS_PIPE_MAP` hands a process, and "nothing the copies
are bounded by is in that page" — so an adversarial write to the mapped page
cannot turn into an out-of-bounds *kernel* access. `a_scribbled_header_leaves_the_stream_exact`
tests exactly that, for the header's `flags` word.

What the doc doesn't say, and what the test doesn't cover: `Ring::read` and
`Ring::write` build `core::slice::from_raw_parts[_mut]` over the **data**
region of that same mapped page (`read.rs:136`, `write.rs:162` in the current
line numbers), and hand the result to the caller as an ordinary `&[u8]` /
`&mut [u8]`. If the pipe's data bytes — not just the header — are reachable
through the same userland mapping, then constructing a Rust shared/exclusive
reference over memory another process can concurrently write violates the
no-concurrent-mutation guarantee those reference types carry, independent of
whether the *kernel's bounds* stay correct. That's a different question from
the one the module answers: not "can userland corrupt an out-of-bounds kernel
read", but "is a plain `&[u8]` the right type for memory that isn't
exclusively ours while the reference is live" — Rust's model gives the
optimizer license to assume no such write happens, which real hardware
tearing doesn't care about but a future codegen change could.

Whether this is real depends on something this file doesn't state and this
pass didn't chase down: is the pipe's *data* region actually part of what
`SYS_PIPE_MAP` maps writable, or does userland only ever see `RingHeader`
(with the data bytes moving exclusively through syscalls the kernel mediates
and copies for)? `kernel::pipe`'s `PIPES` table and whatever builds the
`SYS_PIPE_MAP` mapping are what would answer that, not this crate. If the
data region is shared, the fix is to route these copies through volatile or
relaxed-atomic byte access (or a type that says "outside memory" explicitly)
rather than `&[u8]`; if it isn't shared, the safety comments already written
should say so plainly instead of citing the header-only trust boundary and
the reader should not have had to ask.

**2026-08-25: promoted, and the open question answered.** The data region is
shared: `kernel/src/pipe.rs`'s `Backing` holds one physical page for the whole
ring, `SYS_PIPE_MAP` maps that page (not a header-only sub-window) writable
into the process, and `Pipe`'s own `unsafe impl Send` comment already says so
— "`Ring`'s base cannot become a `&mut [u8]`: `SYS_PIPE_MAP` maps the same
page into the process." `toyos-abi/src/ring.rs`'s current safety comments
(added in the same 2026-08-20 pass this finding came out of) now say
plainly that the data region is not exclusive, so the documentation half of
this is done. The code half is not: `Ring::read`/`Ring::write` still build
`core::slice::from_raw_parts[_mut]` into ordinary `&[u8]`/`&mut [u8]` over
that shared page. This falls in the finding's first branch — the fix is
routing these copies through volatile or relaxed-atomic byte access, or a
type that says "outside memory" explicitly — and is real, unresolved work on
a foundational IPC primitive. Sysroot law: the site is `toyos-abi/src`, so
any fix lands on its own single-commit branch per `abi_lands_alone`.

**2026-08-27: the map re-verified, and the split re-measured.** The premise is
not inference from a comment any more:
`kernel/src/arch/syscall/ipc.rs`'s `sys_pipe_map` calls
`vma_map(&pt, phys.phys(), pipe::PIPE_SIZE as u64, Prot::ReadWrite)` — the
whole `PIPE_SIZE` region, data included, writable, not a header-sized window.

**There is no sysroot-only half of this fix.** `Ring::read`/`Ring::write`
already describe the destination rather than passing it (`sink`/`fill`
closures, so the *kernel's* user page never becomes a `&mut [u8]`), and the
remaining defect is precisely the type those closures receive: a `&[u8]` /
`&mut [u8]` over the shared page. Removing it changes the closure parameter
type, and the entire consumer set is two lines in `kernel/src/pipe.rs`
(`try_read`'s `|off, src| buf.write_at(off, src)` and `try_write`'s
`|off, dst| buf.read_at(off, dst)`), whose bodies then need a raw-pointer copy
that `kernel/src/user_ptr.rs` does not expose today. Landing only the
`toyos-abi` side leaves the kernel not compiling; landing a second, unused
"outside memory" API beside the live one is dead code. So this is one commit
touching `toyos-abi/src/ring.rs` and `kernel/src/pipe.rs` together —
`abi_lands_alone` permits exactly that (it partitions commits, and the ABI
change plus its one inseparable consumer is the case its doc comment names),
and it is what the next attempt should be briefed as. A branch scoped to
sysroot sources alone cannot do it.
