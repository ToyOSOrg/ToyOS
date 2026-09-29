---
status: open
kind: tooling
opened: 2026-09-29
---

# A metal judge reads the log file for a record the kernel writes after init made the file whole

On the T14 a boot's `/log` file ends where init had `logd` make it whole
(`Readback::log_reached_the_stick`, `tests/common/metal.rs`). What the kernel
writes after that — `Rebooting.`, and every line an actuator staged inside the
stop prints — reaches only the black-box page, and comes back in the next
loader pass's `loader.log` under `| log-tail:` or the page's ring. Three
judges still ask `Readback::kernel()` for such a record:

- `machine_reboot` — `b[0].kernel().must_say(bootlog::REBOOTING)`;
- `log_poll_outlives_a_close` — the same line at the end of
  `log_close_survived` (`tests/toyos.rs`);
- `usb_reset_records_the_phase_it_cut` — `power::usb_load_chain`'s
  `kernel.must_say(bootlog::USB_LOAD_RUNNING)`.

## Measured

The full T14 run of `main` at `7e151819`
(EXIT=1):

```
FAIL log_poll_outlives_a_close: "Rebooting." never reached the testcases's kernel log:
FAIL usb_reset_records_the_phase_it_cut: "usb-load: sweeping disk 0" never reached the usbload's kernel log:
FAIL machine_reboot: "Rebooting." never reached the jobcase's kernel log:
```

Each record is on the page, in each boot's `loader.log`:

```
testcases/loader.log:53:| log-tail: [12.838 cpu0] Rebooting.
jobcase/loader.log:52:| log-tail: [1.516 cpu0] Rebooting.
usbload/loader.log:77:| [1.526 cpu0] usb-load: sweeping disk 0 from block 6569336 to 7507812, rewriting each run with the bytes just read from it, until this machine is reset out from under it
```

and in no `kernel.log` of the three. Every other assertion these judges make
before the refused one passed.

## Exit condition

The three judges read each post-flush record off the pass after the reset,
and a T14 run of the three names reaches their verdicts; then this file is deleted.
