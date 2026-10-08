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

**A hypothesis, which no measurement ties to a lost press**: the controller
does not treat a press as a power request until an operating system has run
the firmware's initialisation of it, and stage 1 runs none of it: no `_REG`
telling the controller's device its operation region is served, no `_STA` and
`_INI` walk of the namespace, no query method. If so, the slice of the stage
that runs those in the server is what closes this, though no table defines a
`_Q28`. It accounts for the presses that changed nothing; it does not account
for `ff4945d6d`'s one served press, or for which press is the first one
served.

**Measured on the T14, 2026-10-08: fifteen bytes of the controller's memory
hold one value under ToyOS and another under Linux. At 0x05 the one bit that
differs is bit 0, set under ToyOS and clear under Linux, and bit 0 of 0x05 is
the bit the firmware's own `_INI` for the controller cleared in a dry run,
in the variants of it where a clear can show. At 0x03, the byte its `_REG`
wrote, the bit that differs is not the bit `_REG` wrote.** A scout image, one
commit on `b432ed21c` and never pushed, ran stage 1's server with a press
logged and not acted on, and read all 256 bytes of the controller's memory
with the specification's `RD_EC` command three times: as the server armed, at
12.408 s, and 60 s and 120 s after. It wrote nothing to the controller's
memory: each read wrote the command and an address to the controller's two
ports, and the server drained query 0x4f as stage 1 does. Nobody attended the
boot. Its log carries no press line and no query but 0x4f, counts 0, 55 and
110 SCIs before the three reads, and reads the controller's status with
neither buffer flag set before each. The same 256 bytes were read under Linux
on the same machine three times: the first two minutes after its boot by that
reading's own uptime line, the others a minute apart by the orchestrator's
account. The six samples, the table of them and the scout's log are held by
the orchestrator outside the tree; the counts below were made again from the
six samples for this record.

- **233 offsets read alike in all six samples; 23 do not.**
- **Eight of the 23 move by themselves and say nothing**: 0x27, 0x49, 0x78,
  0x7c, 0x7d, 0x7e and 0xaa change between samples under both systems, and
  0x7a between Linux's.
- **Fifteen hold one value in all three ToyOS samples and another in all
  three Linux samples**: 0x03, 0x05, 0x14, 0x32, the nine from 0x53 to 0x5b,
  0x74 and 0xcb. Nine differ in one bit (0x03, 0x05, 0x14, 0x58 to 0x5b, 0x74,
  0xcb) and six in more.

What the firmware's initialisation would write was read earlier, offline: a
scratch copy of the interpreter at `e7010129f` loaded the T14's tables against
a host that recorded every write and made none, and evaluated `\_PIC(1)`, the
`_STA` and `_INI` walk, the controller's `_REG(3, 1)` and each of the 53 query
methods once. Its logs are held outside the tree too. Eight variants of that
host left a log of every access, and each claim below names the ones it is
read from:

- **Four fixed-answer variants**, where a read never sees a write. Three
  answer every read of the controller with zero: one runs `_REG` before the
  walk, and one answers a chipset register differently, which changes no
  access to the controller. The fourth answers every read of the controller
  with all ones.
- **Four write-seeing variants**, where a read of memory or of the controller
  sees an earlier write to the same place, and a write to `SMI_CMD` forgets
  what was written to the memory page last written. A controller byte nothing
  has written answers zero in three, which differ as the three above do, and
  all ones in the fourth.

Every count of it is one branch under one model, and neither zero nor all
ones is what the machine holds. In all eight the controller's `_INI` wrote
the controller at 0x05 and 0x3A, and four query methods wrote it at 0x06,
0x3A and 0x81. `_REG` read and wrote 0x03 in all four fixed-answer variants,
before the walk and after it, and in the write-seeing variant that ran it
before the walk; in the three write-seeing variants that ran it after the
walk it touched the controller not at all. That its order decides whether
`_REG` writes is the write-seeing variants' finding alone. Against the
samples:

- **0x05 is among the fifteen, by bit 0**: set in all three ToyOS samples and
  clear in all three Linux ones. **Bit 0 of 0x05 is the bit the dry run's
  `_INI` clears.** In the two all-ones variants it read all ones there and
  wrote the byte back with bit 0 alone cleared. In the six zero variants it
  read zero and wrote zero, which shows no bit and contradicts none. So the
  dry run names the bit and the direction, and both are the difference's:
  stage 1, which runs no `_INI`, reads the bit set, and Linux reads it as a
  run of that `_INI` leaves it.
- **0x03 is among the fifteen, by one bit, and it is not the dry run's bit.**
  Wherever the dry run's `_REG` wrote 0x03 it wrote bit 0 set: one over a
  zero read in the four zero variants where it wrote, and all ones over all
  ones in the fixed-answer all-ones variant. Bit 0 reads clear in all six
  samples; the bit that differs is another. Under the write-seeing variants a
  clear bit 0 under Linux is what `_REG` after the walk leaves, since there it
  writes nothing. Under the fixed-answer variants `_REG` sets bit 0 in either
  order, and a clear bit 0 under Linux is then unaccounted for. Nothing read
  says in which order Linux ran them or which model the machine follows. What
  writes the bit that differs is unread.
- **0x3A does not differ**, and could not have: the bit the six zero
  variants' `_INI` set there reads set in all six samples, and in the two
  all-ones variants it wrote all ones over all ones, a set and no clear.
- **0x06 and 0x81 do not differ**, which says nothing: a query method runs
  only when its query comes.

What that supports. Something that ran under Linux and did not run under
ToyOS left the controller's memory different, for the 120 s read at least,
and at 0x05 the difference is the bit the firmware's own `_INI` clears, in
the direction it clears it, where stage 1 never runs that `_INI`. That is the
hypothesis's premise at one bit: the controller under stage 1 is not in the
state the initialisation leaves it in.

What it does not. The bit at 0x05 is named by two variants, both a host that
answers all ones, one branch under one model each. Linux also runs the
vendor's platform driver, which writes the controller, and the samples hold
no list of what was loaded on that boot: any of the fifteen may be its doing
and not the firmware's, bit 0 of 0x05 among them. ToyOS was read on one boot,
and nothing says Linux was read on more, so a difference between boots is not
told from a difference between systems. Nine of the fifteen differ by one
bit, and five of those nine by bit 0 set under ToyOS and clear under Linux,
0x05 among them: the match of bit and direction at 0x05 is what most of the
one-bit differences show, and marks nothing by itself. And no measurement
ties bit 0 of 0x05, the bit
at 0x03 or any other of the fifteen to a lost press: the scout's boot holds
no press, the Linux samples say nothing of one, and nothing was written to
the controller's memory to see what a press then does.

**Its test, before the exit's ten boots**: at the first head whose server
runs `_REG`, the init walk and the query methods on the T14, two readings, in
this order. First the scout of 2026-10-08 is rebuilt on it and reads the
controller's memory three times after the initialisation has run, unattended,
and the record here says two things. One is read from this record alone: bit
0 of 0x05 reads clear in all three. The other is which of the other fourteen
offsets now read as under Linux and which as under stage 1, 0x03 among them.
The tree holds no value of those fourteen, so that is read against the six
samples the orchestrator holds, or, if they are gone, against a fresh reading
under Linux on the same machine, three samples of it as here. One that still
reads as under stage 1 is not the firmware's initialisation's doing. A bit 0
of 0x05 that reads set is recorded before any press is asked for: that bit is
then not `_INI`'s to clear on the machine, whatever the dry run wrote.
Then the scout of 2026-10-07 is rebuilt on it and the owner, asked on demand,
presses once, in window 3 only: the arm that lost its press above. Served
there, the hypothesis stands and the exit below is asked for. Lost there with
the controller's status unmoved, it is refuted, and that goes to the owner,
since nothing else the stage builds is known to change what the controller
does with a press.
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
