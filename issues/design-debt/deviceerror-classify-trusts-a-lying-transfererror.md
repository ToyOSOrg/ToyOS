---
status: open
kind: finding
opened: 2026-08-31
---

# `DeviceError::classify` still lets a foreign type mint `Failed` over a durable refusal

`09250c97` closed `issues/design-debt/bcachefs-deviceerror-failed-still-collapses-a-refusal`
by sealing `DeviceError`'s variants behind a private `Sealed` witness
(`bcachefs/src/block_io.rs:69-100`) and stating the class was now
"unrepresentable in safe code outside the crate rather than merely unused."
Two `compile_fail` doctests back that — the old `.map_err(|_|
DeviceError::Failed)` erasure, and naming `Sealed` directly — and both do
fail to compile.

But `DeviceError::classify<E: TransferError>(err: &E) -> Self`
(`block_io.rs:93-99`) is `pub`, and `TransferError` (`block_io.rs:83-86`) is
a `pub trait` with one method, `refused_before_attempt(&self) -> bool`, that
any external crate can implement to answer however it likes. Built and run
outside the crate (path-dependency on `bcachefs`, 2026-08-31):

```rust
struct Lie;
impl TransferError for Lie {
    fn refused_before_attempt(&self) -> bool { false }
}
let out: Result<(), DeviceError> = Err(7u32).map_err(|_| DeviceError::classify(&Lie));
```

compiles clean and prints `Err(Failed(Sealed(())))` — a hand-minted `Failed`
over what could equally have been a durable `Refused`, with no relation to
any real transfer. The closed issue's literal exit condition — a
*hand-written* `Failed` over a refusal, i.e. the fix-it shape, stops
compiling — is met. What remains needs a deliberate lie in a `TransferError`
impl, not an accidental erasure, but the class the closing commit called
unrepresentable is representable via the one door it left in.

Provenance: adversarial review of PR #343.
