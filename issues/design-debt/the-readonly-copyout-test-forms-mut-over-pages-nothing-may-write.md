---
status: open
kind: defect
opened: 2026-09-29
---

# The read-only copy-out test forms `&mut` over pages nothing may write

`tests/toyos-rust-tests/src/bin/abuse_readonly_copyout.rs` names its target to
`syscall::read` and `syscall::process_stats` through `target()` and a cast
`&mut *(addr as *mut ProcessStats)`, because `toyos_abi::syscall`'s typed
wrappers take `&mut [u8]` and `&mut ProcessStats` and no wrapper takes a raw
address. The `&mut` covers a read-only anonymous map, the binary's own `.text`
and the clock page, and is never written through, so the test rests on the
compiler inventing no store through it, not on a language guarantee.
`tls_dtv_race` forms the same reference.

Owner: `toyos-abi`'s syscall wrappers, which would need a raw-address entry
for a caller that names memory it may not hold a `&mut` to; that is an ABI
change and is not this test's to make.

**Exit condition**: the test issues its calls through a wrapper that takes an
address, and `rg 'from_raw_parts_mut|&mut \*\(' tests/toyos-rust-tests/src/bin/abuse_readonly_copyout.rs`
finds nothing.
