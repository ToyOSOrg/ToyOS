---
status: open
kind: tooling
opened: 2026-09-29
---

# `usbload` seals a panel census that `tests/metal-profile.toml` does not price, so its next metal run reds on it

Evidence, read from the tree at `00d6966a`:
- `tests/metal-profile.toml` prices `boot.usbload.complete_ms`, `back_secs`,
  `stick_secs` and `deadline_lateness_ms`, and no `boot.usbload.panel_max_us`
  or `boot.usbload.panel_us`. Every other boot but `perfdiverge`, which ends in
  a panic and seals none, is priced for both.
- `usbload` is ended by the boot deadline, and `seal_wedge`
  (`kernel/src/drivers/panic_console/mod.rs`) seals `{said}{Census}` on that
  path, so the page after the reset carries `panel: paints=`.
- `tests/common/metal.rs`'s `boot_findings` judges every census a boot carries,
  and `Profile::judge` answers `Unfit::Unpriced` for a name with no row.

Not measured on the T14. No `usbload` run has been read since the census
existed.

**Exit**: `boot.usbload.panel_max_us` and `boot.usbload.panel_us` priced as
`boot.deadlinewedge.*`'s are, since both are ended by the same bound, and a
`usbload` metal run that reads green on both.
