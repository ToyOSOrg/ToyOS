---
status: open
kind: defect
opened: 2026-10-12
---

# The AMD bench's keyboard acks the scancode-set query and never answers it, so the kernel disables it

The AMD bench's i8042 probe reaches the keyboard and stops at the set query. A boot of the bench's demo build logged the step that went silent:

- firmware's config byte reads `0x77`, translate on;
- `0xF5` is acked;
- `0xF0` is acked, then `0x00` is acked;
- no set byte arrives in the 750 ms left of the keyboard stage, and no late byte waits in the output buffer after it.

`main` reads that as `SetQuery::Silent` and disables the keyboard: `i8042: kbd DISABLED - the 0xF0 0x00 set query did not complete` (`kernel/src/arch/x86_64/i8042/mod.rs`). The bench has no other keyboard.

What the same boots measured, which a fix has to keep:

- **`0xF4` shares the stage the wait spends.** Read from the code, its ack is then read against the spent deadline, and it fails. With a deadline of its own, the demo build's `0xF4` was acked and the keyboard resumed scanning.
- **The arming write was not starved.** It ran about 1.35 s into the probe's 2.1 s budget.
- **The wire is set 1.** Under the demo build's assumption, set 2 behind the controller's translation, the bench's volume keys arrived as `E0 30` and `E0 2E`. That assumption is the one the `Refused` arm makes when translate is on, and those bytes are set 1's codes for Volume Up and Down.
- **The wait for the set byte costs 750 ms of every boot.**

Owner: the i8042 driver, `kernel/src/arch/x86_64/i8042/`, with the decision in `kernel/ps2`. The design is #877's A2.

## Exit condition

All of:

- an AMD bench boot reads `i8042: kbd set2+xlat (assumed, …) scanning on, GSI 1 … on`;
- a T14 boot reads its `Refused` arm as `main` does, and QEMU reads `Told(0x41)` as `main` does;
- a host test holds that an exchange that stopped before both acks still disables the keyboard;
- the set byte's wait is bounded by a device response time cited at its site from a hardware document, or, where no document states one, the 750 ms stays recorded in an open issue.
