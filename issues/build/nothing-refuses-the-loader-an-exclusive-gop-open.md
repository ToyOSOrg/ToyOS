---
status: assigned
kind: tooling
opened: 2026-10-01
---

# Nothing refuses the loader an exclusive GOP open

`query_gop` (`bootloader/src/main.rs`) opens `GraphicsOutput` with
`GetProtocol`. An exclusive open calls `Stop` on every driver holding the
protocol BY_DRIVER, the firmware's graphics console among them, so the panel
stops at the GOP query and every later loader line is on serial alone. Only
the comment at the open says so.

`screen_loader_lines` was the one check, and `212516e71` deletes it, red on
`main`'s nightly 36696295750 at `ace064f9d`, `guest (8)`:

```
FAIL screen_loader_lines: the panel carried 26 rows at the GOP query and 41 at the loader's last line, a growth of 15, where the loader printed 14 lines between them, 14 rows at 240 columns
```

`git revert 212516e71` brings it back. #640's head `6e0d7da82` counts the
rows without OVMF's boot logo, the cause it names for that red, and its
nightly 36763711317 passed the test in `guest (10)`.

Held by #660 (`wt/toyos-guestcut`): `clippy.toml` refuses
`BootServices::open_protocol_exclusive`, and the loader's exclusive opens go
through `bootloader/src/exclusive.rs`, bounded by `Exclusive`, which
`GraphicsOutput` does not implement.

**Exit**: #660 lands, and a build fails on an exclusive open of
`GraphicsOutput` in the loader: `cargo run -- --clippy` on
`bs.open_protocol_exclusive::<GraphicsOutput>(gop_handle)`, and the loader's
compile on `exclusive::open::<GraphicsOutput>(bs, gop_handle)`.
