---
status: open
kind: tooling
opened: 2026-09-29
---

# The `usbload` boot measures a panel no `tests/metal-profile.toml` row prices

`tests/metal-profile.toml` prices `panel_max_us` and `panel_us` for every
metal boot but `usbload`, whose sealed page carries the panel census like any
other. `tests/common/metal.rs` fails a measured number with no row, so every
run that flashes `usbload` reds for it, whatever
`usb_reset_records_the_phase_it_cut` — the boot's one rider — says.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1):

```
  usbload: Boot: complete 1166 ms, back in 164 s, the stick enumerated 0 s after that
    the panel painted 10 time(s) and put 8741120 px on the glass
    FAIL the metal suite measured "boot.usbload.panel_max_us" and tests/metal-profile.toml prices no such number; add a row with a ceiling and where it came from, because a measurement with no ceiling cannot fail
    FAIL the metal suite measured "boot.usbload.panel_us" and tests/metal-profile.toml prices no such number; add a row with a ceiling and where it came from, because a measurement with no ceiling cannot fail
```

While `usb_reset_records_the_phase_it_cut` is in `src/redlist.rs` no run
flashes `usbload`, so this red is out of sight rather than gone.

## Exit condition

`tests/metal-profile.toml` carries `boot.usbload.panel_max_us` and
`boot.usbload.panel_us` rows, and a T14 run that flashes `usbload` prints
neither refusal.
