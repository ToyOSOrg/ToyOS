---
status: open
kind: tooling
opened: 2026-09-03
---

# The rule that a kernel hash container's keys are the kernel's own is a text scan, and the compiler could hold it instead

Every container in `kernel/src` whose keys crossed the user/kernel boundary is a
`BTreeMap`/`BTreeSet`, and `src/kernelkeys.rs` declares what is left hashed with
the minter of each key. Two things about that gate are weaker than they look,
both measured by the adversarial review of #377.

**The key-origin column is unaudited prose.** The scan matches types, not
origins: it cannot tell a `FileId` from a path, and the only test over the
column asks that the sentence is not empty. With `created_dirs` put back as a
`hashbrown::HashSet<String>` — the gate's own red — a four-line row claiming
`keys: "a directory name, minted by the kernel"` turned it green. Adding a row
is the cheapest way to close a red here, so **whoever reviews a new row owes the
trace of that key across the boundary**; the row records the claim and checks
nothing. A checker is not the answer — the origin of a key is a whole-program
question — but the reviewer's obligation has to be written down, and it is, in
the module header.

**The scan's non-reach list was wrong in both directions**, and is now four
measured forms with a test each (`the_scan_walks_past_four_measured_forms`): an
import alias (`use hashbrown::HashSet as UserKeyed;` — the `use` writes no
generic arguments and the field then carries neither word; it compiles and
builds an image), generic arguments split across lines as rustfmt produces for a
long type, a turbofish (`HashSet::<String>::new()`, which is written and not
inferred), and a type inferred from `HashMap::new()`. A `type` alias's
*definition* is caught, which the first version of the header denied. This is
the same wall `issues/build/a-spawn-that-is-not-command-is-in-no-ledger.md`
measured for the spawn ledger; do not iterate the matcher.

**The exit condition is one line of `kernel/Cargo.toml`.** Dropping hashbrown's
`default-hasher` feature deletes `DefaultHashBuilder`'s `BuildHasher` impl, so
every `HashMap::new()` in the kernel stops compiling until it names a
`BuildHasher` the kernel owns — every spelling at once, with no scan to walk
past, and the origin column becomes the only thing left to review. What that
costs is the hasher itself: a `BuildHasher` whose `Default` can be constructed
at the point each of the seven remaining maps is built, seeded once from
`RDRAND` (`kernel/src/arch/cpu.rs`) before any of them exists. `Vfs`, the process
table and `SHARES` are all built inside `kernel_main`'s init phase, so "before
any map is built" is a boot-order obligation a wrong answer would make silent —
which is why this is worth its own change rather than a rider.

When it lands, `src/kernelkeys.rs`'s scan goes with it; its `DECLARED` table
does not, because the origin column is the part no compiler can hold.
