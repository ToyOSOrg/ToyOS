---
status: open
kind: defect
opened: 2026-10-28
---

# Linux loses the T14's I219 after a ToyOS warm reset

On 2026-10-28 a T14 boot (Tiger Lake PCH, I219-V `8086:15fc`) ran netstack's
I219 driver, linked at 1000 Mb/s, and rebooted at 201 s through the FADT's
reset register, `0xCF9` ← `06h`: a host reset without a power cycle, which
keeps the PHY powered. Linux's e1000e then logged "Failed to disable ULP" and
its probe failed with -2, a PHY that answered on neither PCIe nor SMBus even
after a `LANPHYPC` power cycle. The NIC stayed absent until AC power was
removed. The same image had booted into Linux cleanly five minutes earlier.
ToyOS's log of that boot is the stick's log partition, kept by the
orchestrator; the failing Linux boot is the T14's journal at 10:51:19 UTC.
ToyOS's own start found the PHY as on every logged boot: silent over PCIe,
silent after its power cycle, answering once the MAC was forced onto SMBus.

**Candidates, none shown:**

- (a) An Ultra Low Power configuration ToyOS never cleared, armed by the agent
  before it, carried through the host reset into Linux. #1091 disarms it at
  start and logs what it found.
- (b) PHY state left by ToyOS's own reach for the PHY: the `LANPHYPC` hold
  was 100 ms before #1091 (now at least 10 µs, released at once), and its PHY
  transactions follow forcing SMBus.
- (c) A PHY that failed on its own.

**For (a): the bench's Linux has magic-packet wake on.** Before the T14 boot
of 2026-10-28 13:22 UTC its e1000e reported "Supports Wake-on: pumbg" and
"Wake-on: g". A Linux driver arms ULP at its own shutdown, reboot and kexec when
a wake is wanted and none of the filters ULP cannot serve (unicast, multicast,
broadcast) is among them, and magic packet is not one of those. So every
Linux reboot into ToyOS on this bench leaves ULP armed, which fits ToyOS's start
finding the PHY silent over PCIe and answering only on SMBus on every logged
boot. Before #1091 nothing in ToyOS cleared it.

**What the T14 read** (2026-10-28 13:37 UTC, `2fb6b8c60`, booted from the
bench's Linux with `Wake-on: g`; row `i219_leaves_ulp_unarmed` passed; the
judged lines are `i219-ulp/r4/judge-i219-lines.txt` at evidence commit
`8a2c4d38fda8e1798c5221aa119f84661a1fd138`):

- The wake's ladder went as-found silent, power-cycled silent, smbus-forced
  answered, back-on-pcie answered. `LANPHYPC` was held low for 10.09 µs.
  `FWSM` read `0x60000040`: firmware not valid, so the driver owns ULP.
- The wake then found ULP Configuration 1 at `0x1800`: only bits 11 and 12
  set, the two `ulp.rs` names (unpublished) as keeping sticky ULP and SMBus
  across `PERST#`. Sticky ULP (bit 4), the entry trigger, and the power-up
  indication were clear. The driver's "armed" means a bit of its
  `config::ARMING` mask was set, not that the PHY was set to enter ULP. Power control read
  `0x9084` (K1 off) and `0xE4` read `0x80000000`, bit 5 already clear.
- After the disarm and `RST|PHY_RST`, the bring-up read ULP Configuration 1
  at `0x0000`, power control `0x9084` again, `0xE4` unchanged.

So bits 11 and 12 survive in the direction Linux → ToyOS: through Linux's
reboot, the host reset, a 10.09 µs `LANPHYPC` hold and the SMBus force, read
before `RST|PHY_RST`. The failure ran the other direction, ToyOS → Linux, and
nothing has read what a ToyOS boot hands across `0xCF9`. Nor do these
readings cover ToyOS's start before #1091, which held `LANPHYPC` for 100 ms
and ran `RST|PHY_RST` and the bring-up with the arming still standing.
#1091 changed both (a), with the disarm, and (b), with the hold, so a clean
failure rate after it says whether the defect persists, not which candidate
caused it.

**K1, not settled.** The disarm writes power control with K1 enabled (page
770 register 17 bit 14), `0x9084 | K1_ENABLE` = `0xD084`, and nothing reads
the register before `RST|PHY_RST`. The bring-up then reads `0x9084`, K1 off,
and nothing in the driver writes the register again, so the link observed
after #1091 runs with K1 off. The cause is undetermined; the readings fit
each of:

- the PHY ignoring the write;
- the `START` commit, given that ULP's entry clears K1 (`ulp.rs`);
- `PHY_RST` returning the register to `0x9084`;
- the MAC's own configuration of the PHY after the reset (`STATUS` bit 9,
  `regs::status::PHY_CONFIGURED`).

One power-control read after the K1 write and one after `START`, in the next
instrumented boot, tell an ignored write and the `START` commit apart from
each other and from the reset. Both reads precede `RST|PHY_RST`, so they
leave the reset's two causes, `PHY_RST` and the MAC's post-reset
configuration, undetermined. Until the cause is known the host model is
contradicted by real hardware: `stub.rs` carries `power_control` across the reset (it is
set only at construction and by the driver's write), and
`ulp_left_armed_is_disarmed_and_a_lost_link_leaves_the_phy_in_reach` in
`tests.rs` asserts K1 on after the bring-up. Both are unconfirmed pending the
cause. Whether this PCH needs K1 off at gigabit is outside what the
clean-room facts cover, and it is open only once the write is shown to take.
The disarm's ULP result does not depend on K1.

**One cut-off trial.** The 13:22 UTC boot, an `i219_leaves_ulp_unarmed` image with
no job of its own at `a6ae98b35`, rebooted 3 ms after netstack started, so the
driver was cut off part way through its wake.

**Instrument.** Since #1091 the wake's line says what ULP held when the PHY
first answered, and the bring-up's what it holds after the reset. Every T14
metal boot ends with `0xCF9` ← `06h` into the loader's chain pass and the
bench's Linux, and the orchestrator's runner checks e1000e's probe after each
one: each boot is one trial of the failing transition. The runner records
every boot locally and publishes the rows periodically, not per boot, to
`nic-trials.tsv` on the evidence repository's `bench-nic-trials` branch;
that file is this issue's trial count. Rows before 2026-10-28T13:20Z carry
run label and result; from then each row is UTC date, `label@revision`,
result (`ok`, `MISSING` or `unreachable`). At evidence commit
`0ea0cb15089f34ecfc3b68c0e36c09fcd5a678a3` it holds both #1091 trials,
`m1091-benchcase@a6ae98b35 ok` (the cut-off trial) and
`m1091b-benchcase@2fb6b8c60 ok` (the reading). An e1000e probe reports only
success or failure: it never reads out the ULP configuration it met, so a
clean probe cannot tell an armed hand-off from an unarmed one.

**Owner**: the I219 driver (`system/i219/driver/`).

**Exit**, in order:

1. The bench's Linux, its Wake-on-LAN setting recorded, reboots into an image
   carrying the instrument: the wake's reading is what Linux and the firmware
   left. Done, in the reading recorded here.
2. Linux reboots into the base image before #1091, which reboots into an
   instrumented image: the wake's reading is what a pre-#1091 start handed
   across `0xCF9`. Then an instrumented image reboots into itself: the
   reading is what the fixed driver hands on.
3. Each T14 boot's e1000e probe after a ToyOS boot, in `nic-trials.tsv`
   (Linux is a black-box measurement here, not a committed oracle). A clean
   trial alone proves nothing for a failure seen once: calling the defect
   fixed needs the base image's failure rate measured first, or, at a rate
   near the one in 176 logged boots, about 500 clean cycles for 95%
   confidence.
