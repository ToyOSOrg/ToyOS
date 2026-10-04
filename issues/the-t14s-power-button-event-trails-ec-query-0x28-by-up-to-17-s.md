---
status: open
kind: defect
opened: 2026-10-04
---

# The T14's power-button event trails EC query 0x28 by up to 17 s

On the T14 in ACPI mode, `/system/bin/acpiserver` takes embedded-controller
query 0x28 on the boots a hand pressed the power button on, and the fixed
power-button event, `PWRBTN_STS`, which is what powers the machine off,
arrives after it. The attended `testcases-press` boots, as `logkeeper` wrote
them to the stick:

| head | query 0x28 first taken | power button pressed | gap |
|---|---|---|---|
| `b3b9ccd69` | 6.696 s | 16.705 s, SCI 19 | 10.009 s |
| `7c3a7dc7d` | 4.804 s | 4.821 s, SCI 7 | 0.017 s |
| `ee6aadecb` | 32.628 s | 50.180 s, SCI 49 | 17.552 s |

Each line is `acpiserver: embedded controller query 0x28 taken for the first
time, served by nothing: stage 1 runs no AML`, followed by `acpiserver: the
power button was pressed, on SCI <n> of this boot; asking the supervisor to
power off`, the supervisor's `(Shutdown)` line at the same millisecond, and
nothing after it. At `ee6aadecb` the driver lost the boot
(`issues/the-metal-driver-reads-a-machine-left-in-s5-as-one-that-did-not-come-back.md`)
and the log was read off a copy of the stick's log partition.

Query 0x28 marks the press only by inference: it appears on those three boots
and on no unattended boot of the same branch (`acpicase`, `testcases`,
`testcases-hold`, `testcases-off`, the `counters` boots), which take 0x4f
alone. The host recorded no time of the press, so whether the button went down
at the query or at the event, and whether the owner pressed once, is not in
any record. The lag predates the merge of `origin/main` at `c4ab2b1e1`.

No table of the T14 defines `_Q28`: a byte search of its DSDT and every SSDT,
dynamic ones included, captured before its wipe and read outside the tree,
finds `_Q4F` in the DSDT and no `_Q28`. Stage 1 runs no AML, so the server
takes 0x28 and serves nothing. What the controller waits on before it raises
`PWRBTN_STS`, if anything, is unread.

Owned by the AML interpreter stages of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, whose
interpreter runs the methods the controller's queries name.

**Exit**: `acpi_power_button_pressed` reds where the server's power-button
line comes more than one second, a policy bound, after query 0x28's
first-sighting line, and it passes on the T14.
