---
status: open
kind: defect
opened: 2026-10-04
---

# The T14's power-button event came up to 17 s after EC query 0x28

On the T14 in ACPI mode, a press of the power button is sometimes lost: the
machine stops only on a later press. On every boot that served a press,
`/system/bin/acpiserver` took embedded-controller query 0x28 first, and the
fixed power-button event, `PWRBTN_STS`, which is what powers the machine off,
arrived after it. The boots that served one, each held open by `acpi_hold`,
as `logkeeper` wrote them to the stick:

| head | query 0x28 first taken | power button pressed | gap | owner's presses |
|---|---|---|---|---|
| `b3b9ccd69` | 6.696 s | 16.705 s, SCI 19 | 10.009 s | two |
| `7c3a7dc7d` | 4.804 s | 4.821 s, SCI 7 | 0.017 s | not recorded |
| `ee6aadecb` | 32.628 s | 50.180 s, SCI 49 | 17.552 s | not recorded |
| `ff4945d6d` | 13.089 s | 13.105 s, SCI 13 | 0.016 s | one |
| `8d7004d3b` | 30.550 s | 30.566 s, SCI 17 | 0.016 s | three |
| `448107b54` | 36.630 s | 40.750 s, SCI 29 | 4.120 s | not recorded |

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
The presses the measurement below lost were cued 5 s, 45 s and 85 s after the
server armed, in neither window.

**Measured on the T14, 2026-10-07: a scout read three lost presses through
PM1 status, GPE0 status, the embedded controller's status register polled
every 10.1 ms and the SMI count and found none of them in any, and the same
cue is served on one boot and lost on the next.** Asked whether he would do
a one-time measurement, the owner answered "Yes, on demand (Recommended)";
the question's scope was one held boot with three presses, each when asked
for, a measurement and not a test. That is the second boot below. After it
the orchestrator proposed one more boot of the same image with a single press
in the third window, to tell count from time, and the owner answered "go":
the third boot. A scout image, built on `e7010129f` and never pushed,
ran stage 1's server with a press logged and not acted on, and held the boot
150 s after the server armed. It showed three cues on the panel, each a 15 s
window, 5 s, 45 s and 85 s after arming. At every SCI, before anything was
cleared, and at a poll wherever a value had changed, the server logged PM1
status, the GPE0 status bytes and the embedded controller's status register;
the poll ran 99 times a second by the counts on the cue lines, a period of
about 10.1 ms. Every 100 ms the kernel sampled every CPU's `MSR_SMI_COUNT`,
`PM1a_CNT`, `SMI_EN` and a word it read as `SMI_STS`, and logged them on a
change and about once a second otherwise. The `SMI_STS` offset was written
from memory of the chipset's datasheet and is unverified for this machine.
Three boots of that image, their logs held by the orchestrator outside the
tree; times are each log's own stamps:

- **Nobody pressing.** Armed at 14.023 s. 136 SCIs in the 150 s: 68 pairs,
  the first of each pair draining query 0x4f and the second draining nothing,
  2.215 s to 2.249 s apart, 2.219 s on average. PM1 status read 0 on every
  line. The SMI count read 4818 on all eight CPUs in every sample. `SMI_STS`
  moved once, 0 to 0x00004000 at 63.746 s, with no count moving. 18, 54 and
  90 SCIs had come when the three windows closed.
- **One press in each window**, the owner's account: "i saw all red boxed and
  i executed the steps perfectly". Armed at 13.265 s; windows at 18.274 s to
  33.269 s, 58.277 s to 73.269 s and 98.273 s to 113.268 s. Windows 1 and 2
  hold what the boot nobody pressed on holds and nothing else: seven pairs of
  0x4f each, 18 and 54 SCIs at their closes, PM1 status 0 on every line, no
  query 0x28, the controller's status never other than its two 0x4f values,
  the SMI count 4818 on all eight CPUs. `SMI_STS` made the same one move, at
  63.602 s. In window 3, 2.1 s after its cue, SCI 79 drained query 0x28 at
  100.382 s and SCI 81 read `PWRBTN_STS` at 100.399 s, 17 ms later, served.
  That is the boot's only 0x28 and its only press line: 137 SCIs, one press.
- **One press, in window 3 only**, the owner's account: "yes i pressed once
  at the third red block only". Armed at 13.338 s; window 3 at 98.346 s to
  113.345 s. The boot holds what the boot nobody pressed on holds, but for
  one poll line: 136 SCIs, 68 pairs of 0x4f, 18, 54 and 90 at the closes, no
  query 0x28, PM1 status 0 on every line, the SMI count 4819 on all eight
  CPUs, `SMI_STS`'s one move at 63.769 s. The poll line: at 114.522 s, 1.18 s
  after window 3 closed, the poll read the controller's status as 0x28, the
  value every SCI that drains 0x4f reads, 166 µs before SCI 91 read the same
  and drained 0x4f. Every other poll line of the three boots reads it as
  0x08, every one in window 3 among them.

On all three, every SCI's line reads the power-button enable set in `PM1_EN`
and every kernel sample reads `SCI_EN` set and `SMI_EN` unchanged.

What that shows. A press cued 85 s after arming was served on the second boot
and lost on the third, so time since boot or since arming does not decide the
loss. By the owner's accounts, what differed is that two presses had come
before it on the second boot and none on the third. By the logs, the SMI
count differed too, from before the kernel's enable: cpu0 read 4818 before
the write of `ACPI_ENABLE` and 4819 after on the third boot, and 4817 and
4818 on the first two. The three presses lost on these boots raised no
`PWRBTN_STS`, no SCI, no query and no SMI, and the poll read the controller's
status as 0x08 on every line of their windows, with the event enabled and the
machine in ACPI mode throughout: they were lost before any of what the scout
read, which is neither the server's arming nor SMM taking the press.
`8d7004d3b`'s boot has the same shape: three presses by the owner's count,
and its one 0x28 came 16 ms before the one press it served.

What it does not show. A count of earlier presses is read off two boots, and
`ff4945d6d` served the only press of its boot. And a second shape of loss is
on the stick's logs and not in this measurement: `b3b9ccd69`, `ee6aadecb` and
`448107b54` each took a query 0x28 with no `PWRBTN_STS` behind it, seconds
before the press that was served, where every boot above that served a press
at once took its 0x28 within 17 ms of it. `448107b54` is a boot of the same
day: armed at 13.056 s, 0x28 alone at 36.630 s, the served press at 40.750 s.
Whether that 0x28 was a press the controller reported and did not latch is
unread.

**Ruled** (owner, 2026-10-05): "Land it, record the gap (Recommended)" —
"Stage 1 lands (ACPI mode, SMIs stop, power-off, port isolation); the missed
first press is recorded as a known weakness with its own exit, and the press
test is fixed to fail when the first press is lost. The AML stage closes it."

**Ruled** (owner, 2026-10-05): "A test that requires manual steps from me is
forbidden."

**Ruled** (owner, 2026-10-05, on what a T14 test may need): "No automated test
is allowed that requires physical buttons to be pressed or anything we cant do
now with the t14. I can test it on demand but no ci there not always someone
available physically".

The two later rulings supersede the first one's "the press test is fixed to
fail when the first press is lost": that test was the `acpi_power_button_pressed`
row, judged on the owner's own count of his presses, and it is deleted. **No
harness row reads the T14's press**: QEMU's `acpi_power_button`
reads that a press the server takes stops the machine, on q35, and the T14's
own button is read only by the owner's hand.

Owned by the stage "the interpreter" of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`, the
first ruling's "AML stage".

**A hypothesis, measured by nothing yet**: the controller does not treat a
press as a power request until an operating system has run the firmware's
initialisation of it, and stage 1 runs none of it: no `_REG` telling the
controller's device its operation region is served, no `_STA` and `_INI` walk
of the namespace, no query method. If so, the slice of the stage that runs
those in the server is what closes this, though no table defines a `_Q28`.
It accounts for the presses that changed nothing; it does not account for
`ff4945d6d`'s one served press, or for which press is the first one served.

**Its test, before the exit's ten boots**: at the first head whose server
runs `_REG`, the init walk and the query methods on the T14, the same scout
is rebuilt on it and the owner, asked on demand, presses once, in window 3
only: the arm that lost its press above. Served there, the hypothesis
stands and the exit below is asked for. Lost there with the controller's
status unmoved, it is refuted, and that goes to the owner, since nothing else
the stage builds is known to change what the controller does with a press.
One boot decides neither a fix nor this issue's close.

**Exit**: an on-demand check by the owner, which the orchestrator asks him
for at the head that claims the fix and which no test or CI job waits on. On
ten boots of that head held open by `acpi_hold` he presses the power button
once, briefly; every boot's log carries the server's press line and the
supervisor's power-off, and he reports one press for each. Each boot ends in
S5 and leaves no readback, so each log is read off the stick's log partition,
copied before the next flash, as at `ee6aadecb`. The head, the ten logs'
lines and his account are recorded here, and then this issue closes.
Ten is not the owner's: a loss rate of one in three would pass ten boots in a
row 1.7% of the time, where `ff4945d6d`'s one boot passed alone.
