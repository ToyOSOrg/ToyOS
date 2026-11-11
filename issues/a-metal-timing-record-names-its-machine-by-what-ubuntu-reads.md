---
status: open
kind: tooling
opened: 2026-09-29
---

# A metal timing record names its machine by what Ubuntu reads of it

A boot names its bench itself: the kernel's first record carries the
processor's signature, and the judge takes the bench, and with it the rows it
judges, from the readback (`toyos_metal::bench::Bench::of_log`). The timing
record does not. `tests/metal/src/lib.rs`'s `run` still asks the operating
system the bench runs between boots for `timings::Machine::QUERY` over ssh
before the flash, and writes the answer into the readback's `boot.txt` under
three keys (`VENDOR_KEY`, `PRODUCT_KEY`, `BIOS_KEY`); `timings::Record::load`
picks the machine's record by them, and a run under another BIOS string is
red.

So every readback carries the machine's vendor, product and firmware strings,
which are masked by hand before one is published, and a resident runner,
tests inside a ToyOS that stays up on the machine with no Ubuntu and no
reboot per test, has nothing to ask. The read before the flash is also the
gate that keeps the loop off a machine with no record, which only the
operating system running then can answer.

## Exit condition

A machine's record is its bench's, found by the bench the boot names; a
firmware change is told from something the boot itself says that identifies
no machine; `boot.txt` carries no vendor, product or firmware string; and the
ssh read is left to the pre-flash gate alone, or goes with Ubuntu.
