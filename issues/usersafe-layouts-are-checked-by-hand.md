---
status: open
kind: defect
opened: 2026-10-07
---

# `UserSafe` layouts are checked by hand, and one impl already states a size its type does not have

`kernel/src/user_ptr.rs`'s `UserSafe` is an `unsafe trait` whose contract is
`#[repr(C)]`, `Copy`, no padding and every bit pattern valid. `copy_in` reads a
`T: UserSafe` out of user memory with `read_volatile` and `copy_out` writes one
with `write_volatile`, so a padding byte in `T` is kernel stack written to
userland, and a field with an invalid bit pattern is undefined behaviour chosen
by the caller. The file says of its own impls that each is hand-checked and that
Rust cannot verify it.

Read at `f260e0b98`, nothing run:

- **The hand check has already drifted.** The `SAFETY` comment on
  `unsafe impl UserSafe for toyos_abi::log::LogCursor` says 88 bytes;
  `toyos-abi/src/log.rs` asserts `size_of::<LogCursor>() == 16 + 8 *
  MAX_LOG_SHARDS` with `MAX_LOG_SHARDS = 8`, and its test asserts 80.
- **Four of the structs carry no size assertion at all**: `Stat`
  (`kernel/src/object/ops.rs`), `SchedInfo`, `ProcessStats` and
  `FramebufferInfo`. `TraceCursor` has none either and needs none, being
  `repr(transparent)` over `LogCursor`. `SpawnArgs`, `NamespaceBuild` and
  `InboxSetup` each have a `const _` on a literal total, which a field added
  together with its new total still satisfies with a gap inside. `LogCursor`,
  `RawKeyEvent` and `MouseEvent` assert a sum of field sizes, which a field
  added with its own size fails on any gap.
- **The second list is a copy.** `toyos-userbound/src/span.rs`'s test table is
  headed "Every `UserSafe` type" and is thirteen name strings with a size and
  an alignment written beside each; it names neither `LogCursor` nor
  `TraceCursor`, and nothing ties a row to the type it names.

No impl is known to be wrong today: every field read is an integer or a
`repr(transparent)` wrapper over one. The defect is that a field added to an
ABI struct in `toyos-abi` compiles clean in the kernel whatever it does to the
layout, on a boundary root `CLAUDE.md` wants unrepresentable before it is
checked.

## Exit condition

The compiler refuses a `UserSafe` impl whose type has a padding byte or a field
that is not valid for every bit pattern, by a derive or by one macro that
writes the impl and its assertions together, so no impl is written by hand.
Shown by two mutations that fail the build: a `u32` appended to `SchedInfo`,
and a `core::num::NonZeroU32` in place of one of `ProcessStats`'s `u32`s, which
has that `u32`'s size and alignment and so fails on bit validity alone.
`span.rs`'s table is
then generated from the same list or deleted, and this file with it. Whether
that is a published crate or the kernel's own is the builder's to argue against
root `CLAUDE.md`'s rule on kernel dependencies.

## Owner

`kernel/src/user_ptr.rs`, `toyos-abi`. Nobody holds it.
