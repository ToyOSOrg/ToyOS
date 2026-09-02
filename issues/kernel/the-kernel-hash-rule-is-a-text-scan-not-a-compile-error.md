---
status: open
kind: defect
opened: 2026-09-03
---

# The rule that a kernel hash container's keys are the kernel's own is a text scan, and the compiler could hold it instead

Every container in `kernel/src` whose keys crossed the user/kernel boundary is a
`BTreeMap`/`BTreeSet`, and `src/kernelkeys.rs` declares what is left hashed with
the minter of each key. That gate is a **text scan**, and its own header says
which spelling it closes: a `HashMap<…>` or `HashSet<…>` written with its
generic arguments, on a non-comment line, in a `.rs` file under `kernel/src`. It
does not reach a `type` alias, a re-export, a map whose type is inferred from
`HashMap::new()`, or any other hashed container — the same wall
`issues/build/a-spawn-that-is-not-command-is-in-no-ledger.md` measured for the
spawn ledger.

**The exit condition is one line of `kernel/Cargo.toml`.** Dropping hashbrown's
`default-hasher` feature deletes `DefaultHashBuilder`'s `BuildHasher` impl, so
every `HashMap::new()` in the kernel stops compiling until it names a
`BuildHasher` the kernel owns — every spelling at once, with no scan to walk
past. What that costs is the hasher itself: a `BuildHasher` whose `Default` can
be constructed at the point each of the seven remaining maps is built, seeded
once from `RDRAND` (`kernel/src/arch/cpu.rs`) before any of them exists. `Vfs`,
the process table and `SHARES` are all built inside `kernel_main`'s init phase,
so "before any map is built" is a boot-order obligation a wrong answer would
make silent — which is why this is worth its own change rather than a rider.

When it lands, `src/kernelkeys.rs` goes with it.
