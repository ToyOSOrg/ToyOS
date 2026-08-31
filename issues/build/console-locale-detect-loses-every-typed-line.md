---
status: open
kind: defect
opened: 2026-08-31
---

# `console_locale_detect`'s ready handshake does not pace between PS/2 bursts

Seen on CI 2026-08-31, pull-request `ci` run 33411831704, job 99553283770
(`guest (1)`), headSha `30918d0e`:

```
FAIL console_locale_detect: 10 typed lines and none of them came back
  FAIL  console_locale_detect  (30s)
  ALONE console_locale_detect: GREEN, and it was alone both times — nothing the
  harness controls differed, so it failed once and passed once. That is a rate
  and not a classification.
```

The shard was otherwise green: `196 passed, 1 failed, 197 total (144.8s)`.

**Not about the branch it appeared on.** `ci-qemu-pin`'s whole delta is
`.github/` and `src/ci.rs` — it repoints the hosted jobs' apt source at a dated
Debian archive so a rolling release cannot move the QEMU version, and it adds
two gates over that. No kernel, userland, or harness byte, and the QEMU the
guest ran is the same 11.1.0 the declaration names.

**Which path failed.** The message is not `shell_type_line`'s three-attempt
verdict. It is emitted at `tests/toyos.rs:6292` by `shell_echoes`, called by
`shell_answers` before this test types `locale detect` (`tests/toyos.rs:6771-6775`).
The ten are ten direct calls to `shell_type_once` (`tests/toyos.rs:6278-6285`),
not ten lines each retried three times. The retired row can therefore still be
right about its original, later `locale detect` failure; today's failure is the
ready handshake that precedes it.

The retirement nevertheless exposes the live hole. `shell_type_once` splits
the 21-character `echo surface-up-zqjxk` into queue-sized bursts and then sends
every burst plus Enter back-to-back (`tests/toyos.rs:6194-6201`). Those
characters and Enter are 44 set-1 bytes under `scancode_bytes`
(`tests/common/qemu.rs:3368-3377`), against a 16-byte device queue. Waiting for
QMP's `"return"` only proves that QEMU's main loop accepted an
`input-send-event` command (`tests/common/qemu.rs:3204-3207`,
`tests/common/qemu.rs:3280-3287`); nothing in this path observes that the guest
vCPU ran and drained the previous burst. The claim at `tests/toyos.rs:6154-6157`
that a QMP reply gives the vCPU a turn is the unsupported step.

That mechanism accounts for all ten, rather than merely a random missing byte.
If the vCPU takes no turn across one attempt's QMP commands, the first burst can
fill the device queue and the later bursts and Enter can be dropped. The guest
then drains only an unterminated prefix. Its surface mirror is line-buffered, so
the host sees no line; every retry repeats the same overfill shape. `console:
ready` was already seen, and it is printed after `/bin/console` has acquired the
keyboard and spawned the shell (`userland/console/src/main.rs:121-131`), so
"the keyboard was never lent" is not the leading hypothesis.

There is one live alternative: all 44 bytes may reach the i8042 and the console
event loop may fail to read or forward them. The kernel counts bytes as it takes
them from the ring (`kernel/src/drivers/i8042/mod.rs:698-770`), and the console
only forwards translated presses when its keyboard poll token fires
(`userland/console/src/main.rs:215-253`), so those cases are distinguishable.

**Discriminating experiment; do not fold it into an ordinary suite rerun.** In a
test-only branch, boot only `console_locale_detect` with `i8042-trace`, then,
after `console: ready`, run three labelled arms on fresh boots:

1. the current unpaced `echo surface-up-zqjxk` handshake (44 set-1 bytes);
2. `echo q` plus Enter as one batch (14 set-1 bytes, below the 16-byte queue);
3. the same long handshake, but wait after each burst until the raw console's
   decoded input row contains that burst, exactly as `console_type_line` does.

For every arm retain the `i8042: drain bytes=… keys=…` lines, the decoded input
row, and `ConsoleStream::since(mark)`. Run the filtered command
`cargo test --test toyos-build -- console_locale_detect`; do not run the guest
suite. If only arm 1 fails and its drains total less than 44 while arms 2 and 3
pass, the missing guest-side inter-burst acknowledgement is the mechanism. If
all 44 bytes and their key events drain but neither long arm reaches the input
row, the defect is in the console poll/forward path. If no bytes drain in either
arm, the defect is below QMP in i8042 delivery. Fix the branch selected by those
observations, prove the other two controls stay green, and only then retire the
standing row.
