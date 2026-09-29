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

`hda_tone` and `hda_client_stall` red on the T14 run of `main` at `7e151819`
(`issues/build/a-readbacks-kernel-records-never-count-as-kernel-output.md`).
With a `hda_tone` row added to `src/redlist.rs`, `cargo run -- --known-red
hda_tone` answers `YES, disabled — it does not run.` and
`cargo test --test toyos-build -- --list` exits 1 with:

```
[toyos] src/redlist.rs: hda_tone is disabled and nothing registers it: a renamed or deleted test takes its row with it
```

The same tree without that row lists and exits 0.

## Exit condition

`check_redlist` accepts a name `METAL` registers as well as one `schedule`
does, and `cargo run -- --known-red hda_tone` can answer YES from a row the
suite accepts.
