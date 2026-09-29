---
status: open
kind: tooling
opened: 2026-09-29
---

# A readback's kernel records never count as kernel output, so every metal judge that asserts an absence reds unjudged

`Serial::alive` (`tests/common/serial.rs`) counts the lines `qemu::is_kernel_line`
accepts, and that predicate is `line.starts_with("[kernel ")` — the head
`klogd` gives a record on the virtio console. The stick's `/log` file spells
the same records `[<date> <time> <secs> cpuN]`, so on a `metal::Readback` the
count is zero whatever the boot wrote. `must_not_say` and `must_be_clean` call
`alive()` first, so every metal judge that asserts an absence on
`Readback::kernel()` or `Readback::log()` refuses before it judges anything.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), all four off one `testcases` boot that reached `Boot: complete` in
1165 ms:

```
FAIL klogd_hosted: the testcases's kernel log carried no kernel output at all (56038 bytes): every assertion below it would be a claim about nothing
FAIL hda_tone: the testcases's log carried no kernel output at all (69292 bytes): every assertion below it would be a claim about nothing
FAIL hda_client_stall: the testcases's log carried no kernel output at all (69292 bytes): every assertion below it would be a claim about nothing
FAIL loader_watchdog_arms: the testcases's kernel log carried no kernel output at all (56038 bytes): every assertion below it would be a claim about nothing
```

The readback's first line
(`/Users/jan/Dev/jan/toyos-metalmain/target/metal/testcases/kernel.log`):
`[2026-09-29 11:11:20 0.000 cpu0 boot] panic console: armed 1920x1080 ...`.

The callers: `klogd_hosted` (`must_be_clean`), `audio::tone_on_metal` and
`audio::client_stall_on_metal` (`must_not_say`), `power::watchdog_quiet`
(`kernel.must_not_say`). `hda_tone` and `hda_client_stall` are `METAL_ONLY`
rows no schedule registers, so `src/redlist.rs` cannot hold them
(`issues/build/a-metal-only-row-cannot-be-disabled.md`).

## Exit condition

`Serial::alive` recognises a kernel record in the spelling a readback carries,
and a T14 run of the four names above reaches each judge's own assertions; then this file is deleted.
