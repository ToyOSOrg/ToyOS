---
status: open
kind: defect
opened: 2026-10-04
---

# The T14's power-button event came up to 17 s after EC query 0x28

On the T14 in ACPI mode, a press of the power button is sometimes lost: the
machine stops only on a later press. `/system/bin/acpiserver` takes
embedded-controller query 0x28 on the boots a hand pressed the power button
on, and the fixed power-button event, `PWRBTN_STS`, which is what powers the
machine off, arrives after it. The boots a hand pressed the button on, each
held open by `acpi_hold`, as `logkeeper` wrote them to the stick:

| head | query 0x28 first taken | power button pressed | gap | owner's presses |
|---|---|---|---|---|
| `b3b9ccd69` | 6.696 s | 16.705 s, SCI 19 | 10.009 s | two |
| `7c3a7dc7d` | 4.804 s | 4.821 s, SCI 7 | 0.017 s | not recorded |
| `ee6aadecb` | 32.628 s | 50.180 s, SCI 49 | 17.552 s | not recorded |
| `ff4945d6d` | 13.089 s | 13.105 s, SCI 13 | 0.016 s | one |
| `8d7004d3b` | 30.550 s | 30.566 s, SCI 17 | 0.016 s | three |

Each line is `acpiserver: embedded controller query 0x28 taken for the first
time, served by nothing: stage 1 runs no AML`, followed by `acpiserver: the
power button was pressed, on SCI <n> of this boot; asking the supervisor to
power off`, the supervisor's `(Shutdown)` line within a millisecond, and
nothing after it. At `ee6aadecb` the machine stayed in S5 past the driver's
bound, which leaves no readback
(`issues/no-t14-row-reads-the-power-off-after-the-kernels-own-acpi-enable.md`),
and the log was read off a copy of the stick's log partition.

The owner's accounts: at `b3b9ccd69`, nothing happened at his press and a
second press about 5 s later turned the machine off at once (#713's body). At
`ff4945d6d`, he pressed once, about 10 s after the panel showed the loader's
last line, and the machine went off almost at once. At `8d7004d3b`: "the
button press test was not good. nothing happened after the first press. i
pressed it twice after a few seconds only then did it shut down".

**The `8d7004d3b` boot refutes the reading that a lost first press raised
0x28.** Its server armed at 14.363 s and took its first SCI, query 0x4f, at
15.606 s; then nothing until 0x28's first sighting at 30.550 s, 16 ms before
the press it served, on SCI 17. The lost press raised no 0x28, or 0x28's first
sighting would be earlier. Every SCI's `take` reads PM1 status, and the
unattended `testcases-hold` boot of that head logs 28 SCIs by 43.319 s, about
one a second, so a `PWRBTN_STS` latched while the server held the machine
would have been read within about a second: the lost press set no
`PWRBTN_STS` the server could read. What 0x28 marks, and what the
controller or the firmware waits on before it latches `PWRBTN_STS`, is
unread. No table of the T14 defines `_Q28`: a byte search of its DSDT and
every SSDT, dynamic ones included, captured before its wipe and read outside
the tree, finds `_Q4F` in the DSDT and no `_Q28`.

**Two windows lose a press by construction**, apart from the loss above.
Until the kernel mints the server's claim the firmware has the machine in
legacy mode, and what it does with a press there is its own. The kernel
writes `ACPI_ENABLE` at the mint, and the server's start then clears every
latched status, `PWRBTN_STS` among them, before it enables the button
(`userland/acpiserver/src/main.rs`): a press between the enable and that
clear is dropped. On the `8d7004d3b` boot the enable is logged at 14.361 s
and the server's `armed:` line at 14.363 s. The host recorded no press's
time, so whether that boot's lost press fell in either window is not known.

**Ruled** (owner, 2026-10-05): "Land it, record the gap (Recommended)" —
"Stage 1 lands (ACPI mode, SMIs stop, power-off, port isolation); the missed
first press is recorded as a known weakness with its own exit, and the press
test is fixed to fail when the first press is lost. The AML stage closes it."

**Ruled** (owner, 2026-10-05): "A test that requires manual steps from me is
forbidden."

**Ruled** (owner, on what a T14 test may need): "No automated test is allowed
that requires physical buttons to be pressed or anything we cant do now with
the t14. I can test it on demand but no ci there not always someone available
physically".

The two later rulings supersede the first one's "the press test is fixed to
fail when the first press is lost": that test was the `acpi_power_button_pressed`
row, judged on the owner's own count of his presses, and it is deleted. **No
harness row reads the T14's press**, and none will: QEMU's `acpi_power_button`
reads that a press the server takes stops the machine, on q35, and the T14's
own button is read only by the owner's hand.

Owned by the stage "the interpreter" of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, the
first ruling's "AML stage".

**Exit**: an on-demand check by the owner, which the orchestrator asks him
for at the head that claims the fix and which no test or CI job waits on. On
ten boots of that head held open by `acpi_hold` he presses the power button
once, briefly; every boot's log carries the server's press line and the
supervisor's power-off, and he reports one press for each. The head, the ten
logs' lines and his account are recorded here, and then this issue closes.
Ten is not the owner's: a loss rate of one in three would pass ten boots in a
row 1.7% of the time, where `ff4945d6d`'s one boot passed alone.
