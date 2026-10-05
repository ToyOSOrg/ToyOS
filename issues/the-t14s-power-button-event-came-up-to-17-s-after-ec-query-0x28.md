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
machine off, arrives after it. The attended `testcases-press` boots, as
`logkeeper` wrote them to the stick:

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
nothing after it. At `ee6aadecb` the driver lost the boot
(`issues/the-metal-driver-reads-a-machine-left-in-s5-as-one-that-did-not-come-back.md`)
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
`PWRBTN_STS` the server could read. The host recorded no press's time, so
whether it came before the server armed, while the firmware still had the
machine in legacy mode, is not known. What 0x28 marks, and what the
controller or the firmware waits on before it latches `PWRBTN_STS`, is
unread. No table of the T14 defines `_Q28`: a byte search of its DSDT and
every SSDT, dynamic ones included, captured before its wipe and read outside
the tree, finds `_Q4F` in the DSDT and no `_Q28`.

**Ruled** (owner, 2026-10-05): "Land it, record the gap (Recommended)" —
"Stage 1 lands (ACPI mode, SMIs stop, power-off, port isolation); the missed
first press is recorded as a known weakness with its own exit, and the press
test is fixed to fail when the first press is lost. The AML stage closes it."

Owned by the AML interpreter's stages of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, the
ruling's "AML stage"; stage 1's exit carries the lost press here.

**Exit**: `acpi_power_button_pressed`, whose judge reds where the owner's
`presses.txt` records more than one press, passes on ten consecutive attended
T14 boots of one head with no red among them. Ten is not the owner's: a loss
rate of one in three would pass ten boots in a row 1.7% of the time, where
`ff4945d6d`'s one lucky boot already passed the exit this replaces.
