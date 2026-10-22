---
status: open
kind: tooling
opened: 2026-10-22
---

# fileserver and diskserver hold RefCells across awaits, and no clippy shape lints them

The block I/O under the file protocol is async (`toyos-blockdev`), and the
storage design requires clippy's `await_holding_refcell_ref` wherever a
`RefCell` meets those awaits. The lint is on by default. The gate's
`--workspace` shape excludes every program (`build/hostws.rs`'s
`guest_packages`), though, and `build/clippy.rs` has no other shape that
reaches these programs. So the lint never runs on the two programs that hold
the most RefCell-and-await code:

- **fileserver**: `src/cache.rs`'s `Cache`, the async device every volume
  goes through.
- **diskserver**: `src/disk.rs`'s `Served`, the client library's async
  device over a session.

update's install, `Cached` among it, is linted, because `update-copies` is a
host member that compiles that module under `--all-targets`. Its `main.rs` is
not linted.

`issues/userland-programs-are-never-linted.md` does not cover these: its exit
names the crates that `build/userlandhost.rs`'s survey gates, and all three
programs carry an `exempt` manifest.

**Evidence.**
- The lint bites when it is run. A `RefCell` borrow held across
  `disk.flush().await` in `Cache::flush` makes `cargo clippy -p fileserver
  --lib -- -D warnings` exit 101 with `await_holding_refcell_ref` (PR #1025's
  `mut-refcell` control).
- Nothing runs it, and the programs are not clean today. `cargo clippy -p
  fileserver --all-targets -- -W warnings` reports 14 findings on lines older
  than #1025: `collapsible_if` most often, plus `large_enum_variant` on
  `data::Probed` and `len_without_is_empty` on `volume::Out`.
  `-p diskserver` reports one (`manual_is_multiple_of`, `src/session.rs`).

**Owner:** the storage track. C2 puts several tasks on fileserver's executor,
so it needs this lint before it lands.

**Exit:** a shape in `build/clippy.rs`, which both `--clippy` and `--ci host`
run, lints fileserver's and diskserver's host-built targets (and update's
`main.rs`, if it can build on the host) with warnings denied, and their
findings are fixed. A `RefCell` borrow planted across an await in
`Cache::flush` reds `--clippy`.
