---
status: open
kind: defect
opened: 2026-08-31
---

# `DeviceError::classify` still lets a foreign type mint `Failed` over a durable refusal

Commit `09250c97` closed the design-debt entry over a hand-written
`DeviceError::Failed` collapsing a refusal by sealing `DeviceError`'s
variants behind a private `Sealed` witness (`bcachefs/src/block_io.rs:69-100`)
and stating the class was now "unrepresentable in safe code outside the
crate rather than merely unused." Two `compile_fail` doctests back that —
the old `.map_err(|_| DeviceError::Failed)` erasure, and naming `Sealed`
directly — and both do fail to compile.

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

## Sealing the trait is not the exit, measured

`pub trait TransferError: sealed::Sealed` — the same private-witness shape
the variants use — was applied and built. `rustc` refuses the crate's own
external implementors:

```
error[E0277]: the trait bound `Attempted: bcachefs::block_io::sealed::Sealed` is not satisfied
    --> bcachefs/tests/integration.rs:1001:34
     |
1001 | impl bcachefs::TransferError for Attempted {
     |                                  ^^^^^^^^^ unsatisfied trait bound
error[E0277]: the trait bound `OnBudget: bcachefs::block_io::sealed::Sealed` is not satisfied
    --> bcachefs/tests/integration.rs:1009:34
```

and by the same rule would refuse `kernel/src/bcachefs_adapter.rs:24` (the
kernel's `block::BlockError`) and `tests/common/storage.rs:267` (the host
harness's file-backed device). Implementing `TransferError` from outside is
what the trait is *for*: a foreign block device is the only authority on
whether its own transfer was attempted. Making `classify` crate-private has
the identical problem, because those three are the callers.

## What would close it

Give `BlockIO` an associated error type — `type Error: TransferError`, with
`read_block`/`write_block`/`sync` returning `Result<(), Self::Error>` — and
make `classify` crate-private. A foreign implementation then returns *its own*
error and never names `DeviceError` at all, so there is no public constructor
to mint one from nothing; the crate does the classification at the one place
it calls the trait. A lying `refused_before_attempt` is still possible and is
not closable by types — the device's word is the definition — but minting a
verdict with no transfer behind it stops being expressible.

Not attempted here: `DeviceError` threads through `bcachefs/src/fs.rs`,
`btree.rs` and `alloc_bitmap.rs` and six `BlockIO` implementations across the
crate, the kernel and the harness, so the change is a filesystem-wide
refactor and wants the two named checks a filesystem change owes.

Provenance: adversarial review of PR #343; the seal measured 2026-09-02.
