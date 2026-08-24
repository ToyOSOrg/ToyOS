---
status: open
kind: defect
opened: 2026-08-14
---

# A record's one formatter drops `tid=0`, and the first thread of every process is `Tid(0)`

`toyos_abi::log::LogRecord`'s `Display` — the one formatter every consumer
renders through — writes the thread only when it is non-zero:

```rust
if self.tid != 0 {
    write!(f, " tid={}", self.tid)?;
}
```

So zero is the field's "no thread here". **It is also a real thread.**
`ProcessEntry::new` returns *"the allocated main tid (always `Tid(0)` for the
first thread)"* (`kernel/src/process.rs`), and tids are per-process, so `Tid(0)`
is the main thread of every process on the machine. Counted 2026-08-14 over
every `tid=` in the T14 boot logs then committed: **738 `tid=0` against 49
`tid=1`** — the value the formatter drops is the one almost every line carries.

The kernel's own sentinel is a third value again: `PerCpu::current_tid` is
`u32::MAX` when no thread is running (`kernel/src/arch/percpu.rs:85`), which the
formatter would render as `tid=4294967295` on every line a kernel thread logs.

## What is in the tree now

The kernel translates at the boundary: `kernel/src/log/mod.rs`'s `on_a_thread`
maps `u32::MAX` to zero, so nothing ever prints the raw sentinel. That closes
the loud half and leaves the quiet one — a main thread and a kernel thread
render identically, where the byte ring's prefix distinguished them (`[kernel
0.123 cpu0 tid=0]` against `[kernel 0.123 cpu0]`).

**Re-read 2026-08-24, and the distinction is now lost everywhere rather than on
the panel alone.** The byte ring is gone (`kernel/src/log/mod.rs`: "the byte
ring is gone, and the line the console carries is rendered from the record by
`log::console`, through the one formatter in `toyos-abi`"), and
`console::write_line` is public precisely so that `/log`'s sink renders the
same line — `logd` puts a wall-clock prefix in front of the same `Display` and
changes nothing else. Serial, panel and file are one rendering, and it is the
one that drops `tid=0`.

## Why it was not fixed there

`toyos-abi/src/log.rs` is a sysroot source (`src/toolchain.rs`'s
`SYSROOT_SOURCES`), so an ABI change lands on its own pull request and the
kernel-side commit could not carry one.

## The options

1. **A `flags` bit.** `LogRecord::flags` has one bit used (`FLAG_EARLY`); a
   `FLAG_NO_THREAD` says "this record has no thread" out loud and leaves `tid`
   meaning only what it says. Costs nothing on the wire and matches how the
   early-boot label is already carried.
2. **Render `pid`/`tid` together.** The record already holds `pid` and no
   consumer prints it, which is its own smell: a per-process tid is only an
   identity beside the pid it belongs to. `[0.123 cpu0 3/0]` names a thread;
   `tid=0` names one only if you know the process.
3. **Renumber tids from one.** Cheapest to render and the worst of the three —
   it puts an ABI rendering decision inside the process table.

Option 1 or 2, on the next ABI-only landing.
