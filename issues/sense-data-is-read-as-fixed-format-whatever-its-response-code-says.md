---
status: open
kind: defect
opened: 2026-10-09
---

# Sense data is read as fixed format, and as the current command's, whatever its response code says

`toyos_xhci::scsi::Sense::of` takes the sense key from byte 2 and the ASC and
ASCQ from bytes 12 and 13 of any fourteen bytes a device answers REQUEST SENSE
with. SPC-4 makes byte 0's response code say what the rest is: 0x70 current
and 0x71 deferred in fixed format, 0x72 and 0x73 in descriptor format, where
byte 2 is the ASC. So a descriptor-format answer is read from the wrong bytes,
and a deferred error, which is about a command that already completed, is read
as the current command's. The additional sense length in byte 7 is not held to
reach byte 13 either.

Where it costs: `scsi::flushed` turns a SYNCHRONIZE CACHE refused with INVALID
COMMAND OPERATION CODE into `Flushed::NoCache`, the claim that every completed
write is durable. A descriptor-format sense whose bytes 2, 12 and 13 read
0x05, 0x20 and 0x00 makes that claim for a flush that failed. REQUEST SENSE
is sent with DESC 0, so such an answer is outside the standard, which is the
input the decoder exists to refuse. Found by reading the module; no device in
reach is known to answer this way.

Owned by usbd, the small-kernel track's step 10
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`), whose
mass storage stands on this module.

**Exit condition.** `Sense::of` refuses by name a response code other than
0x70 or 0x71 and an additional sense length short of byte 13, carries a
deferred error as one, and a host test holds each against SPC-4's response
code table.
