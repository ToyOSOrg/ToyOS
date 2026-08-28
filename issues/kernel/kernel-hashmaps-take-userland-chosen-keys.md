---
status: open
kind: defect
opened: 2026-08-28
---

# Every kernel HashMap hashes with foldhash's address-derived seed, and `mkdir` lets userland pick the keys

`kernel/Cargo.toml:368` takes hashbrown's `default-hasher` feature, so every `hashbrown::HashMap`/`HashSet` the kernel builds with `HashMap::new()` gets `foldhash::fast::RandomState` — `hashbrown-0.16.1/src/hasher.rs:1-30` wraps it with no indirection, and nothing in the kernel names a `BuildHasher`: `git grep -nE "with_hasher|BuildHasher|RandomState|FoldHasher" kernel/src` is empty. hashbrown declares `[dependencies.foldhash] default-features = false`, so foldhash's `std` feature is off in this build and nothing in `kernel/Cargo.lock` unifies it back on.

That matters because foldhash's two seeds are then addresses and nothing else. `foldhash-0.2.0/src/seed.rs:140-174` mixes exactly three inputs into the shared global seed — a stack address (`:148`), `generate_global_seed`'s own function pointer (`:149`), and `&GLOBAL_SEED_STORAGE` (`:150`) — and the crate's comment at `:143-146` says why: "Use address space layout randomization as our main randomness source. This isn't great, but we don't advertise HashDoS resistance in the first place." The wall clock and allocator address that would augment it sit under `#[cfg(feature = "std")]` at `:157-171` and are not compiled. The per-map seed (`seed.rs:22-71`) is a stack address folded with a racy global counter. foldhash's own front page names this attacker by description (`foldhash-0.2.0/src/lib.rs:5-9`).

**ToyOS has no address space layout randomization for that to lean on.** `grep -rniE "kaslr|aslr|randomiz" kernel/src/ bootloader/src/` returns nothing. All three global-seed inputs are link-time constants of a built image, and `Vfs::new()` (`kernel/src/vfs.rs:124-131`) runs at a fixed point in boot, so the per-map seed is fixed too. Both seeds are the same on every boot of the same kernel.

## The keys are userland's, and `mkdir` is the cheapest path

`mkdir` touches no filesystem at all:

- `kernel/src/arch/syscall/dispatch.rs:293` copies the raw user path and calls `sys_mkdir` (`kernel/src/arch/syscall/fs.rs:138`).
- `resolve_for_modify` (`fs.rs:37-42`) applies one check, `Vfs::user_may_modify` (`kernel/src/vfs.rs:142-145`): `self.mounts.get(&mount).is_none_or(|m| m.access == UserAccess::ReadWrite)`. True for `/tmp` (`kernel/src/main.rs:427`), and true for a mount name that does not exist at all.
- `Vfs::create_dir` (`kernel/src/vfs.rs:447-452`) then does one thing: bounds the path at `MAX_PATH` = 4096 (`vfs.rs:91`) and `self.created_dirs.insert(String::from(path))` into the `hashbrown::HashSet<String>` at `vfs.rs:87`. No mount has to exist, no directory has to exist, nothing reaches a device — and **nothing bounds the entry count**.
- The insert runs under the single global `static VFS: Lock<Option<Vfs>>` (`vfs.rs:13`), held across the check and the mutation by design (`fs.rs:36-42`), with preemption disabled (`kernel/src/sync.rs:56`).

## Impact

A key set sharing the low bucket-index bits degrades hashbrown's group probe to a linear scan, so `created_dirs` insertion goes quadratic while one CPU holds the VFS lock. Every other CPU's `vfs::lock()` spins, and `kernel/src/sync.rs:72-76` **panics** — `panic!("DEADLOCK at {}: 500M spins, ticket={} now={}")` — once a waiter reaches 500M spins. The terminal state of a collision flood is therefore not a slow filesystem; it is a kernel panic raised on a CPU that did nothing wrong, from a syscall an unprivileged process is entitled to make. That is the doctrine line: input that crossed a trust boundary is never trusted and never panics the kernel.

## Precondition and repro

An unprivileged process holding no handle it was not given. `mkdir("/tmp/<key>")` in a loop over keys precomputed offline. Deriving the seeds needs the three addresses in `generate_global_seed`, which are fixed properties of the built image, plus the count of `RandomState`s constructed before `vfs::init()` — also fixed. **Constructing an actually-colliding key set against foldhash's `folded_multiply` was not done here**; that is the one unproven link, and it is why this is a design gap with a concrete reachable consequence rather than a demonstrated exploit. The design gap is the defect: the kernel's hash distribution is a function of data that crossed a trust boundary, and nothing refuses it.

## Two things that travel with it

- **The same set is unbounded in count.** `create_dir` has no ceiling, so `mkdir` of distinct 4 KiB paths grows a kernel `HashSet<String>` until the allocator gives out — `issues/kernel/no-alloc-error-handler.md` is where that ends. This finding is about *distribution*; that one is about *size*. Bounding the count does not fix the hash, but it raises the floor on both, and `vfs.rs:458`'s `created_dirs.retain(|d| !d.starts_with(&prefix))` in `remove_dir` is an O(n) scan under the same lock that wants the same bound.
- `kernel/src/loader/symbols.rs:22-44` builds `HashMap<&str, UserAddr>` from every defined name in a spawned ELF's `.dynsym`/`.symtab` (`kernel/src/loader/mod.rs:452-454`), keys chosen by whoever wrote the binary, bounded only by `MAX_SYMBOL_BYTES` = 16 MiB (`symbols.rs:68`). Narrower — the cap bounds the flood and the cost lands on the spawning process's own load — but the keys are equally attacker-chosen.

## Fix direction, cheapest first

- **Make the vanilla hasher unrepresentable rather than checked.** Drop `default-hasher` from `kernel/Cargo.toml:368` and the compiler enforces it: `DefaultHashBuilder`'s `BuildHasher` impl is `#[cfg(feature = "default-hasher")]` (`hashbrown-0.16.1/src/hasher.rs:19-20`), so every `HashMap::new()` stops compiling until it names the kernel's own hasher. A type alias is then the only way to build a map, and no future `HashMap::new()` can reintroduce the default.
- **Seed that builder from `RDRAND`**, which the kernel already reaches — `kernel/src/arch/cpu.rs:43`, consumed at `kernel/src/arch/syscall/io.rs:263,268` — at the one point in boot before any map is built. `foldhash::fast::SeedableRandomState` takes both seeds explicitly, so this is a seeding change, not a hash change.
- **For the maps whose keys are wholly userland's** — `created_dirs`, and the symbol maps — either a keyed construction whose collision cost is a preimage problem rather than a search, or a `BTreeMap`, whose worst case is logarithmic and whose ordering `remove_dir`'s prefix `retain` (`vfs.rs:458`) would also want.
- **Independently, bound `created_dirs`** and refuse with `ResourceExhausted`, the way `MAX_LIST_ENTRIES` (`vfs.rs:94`) already does for `Vfs::list`.

## The two checks this owes

The negative control is the whole hasher swap reverted onto the base the green arm was measured on, replayed with the same key set: the flood's wall time and the `LOCK CONTENTION` line count (`kernel/src/sync.rs:68`) must come back. The independent oracle is not a second agent — it is foldhash's own documentation (`foldhash-0.2.0/src/lib.rs:5-9`, which describes this attacker in the crate's own words) plus a differential run of the same key set through the replacement hasher, showing the bucket distribution the flood was built to defeat.
