---
status: open
kind: tooling
opened: 2026-09-29
---

# A metal-only row cannot be disabled, so its red has no legal home

`tests/toyos.rs`'s `check_redlist` accepts a `src/redlist.rs` row only for a
name `schedule` holds: the shared boot's discovered binaries, `SCREEN_TESTS`
and `MACHINE_TESTS`. A `METAL` row listed in `METAL_ONLY` has no QEMU
registration and is in none of them, yet the metal branch filters `METAL`
rows through the same `keep` that reads the redlist. A metal-only red can
therefore be neither fixed by a row nor declared by one: every run refuses the
row before any boot.

## Measured

`hda_tone` and `hda_client_stall` red on the T14 run of `main` at `7e151819`.
Only the code reading is verified: `check_redlist` tests `schedule.contains`, and
the metal branch filters `METAL` rows through the same `keep`. Running
`--known-red hda_tone` and `--list` against a tree with such a row is owed and
not in this tree's record.

## Exit condition

`check_redlist` accepts a name `METAL` registers as well as one `schedule`
does, and `cargo run -- --known-red hda_tone` can answer YES from a row the
suite accepts.
