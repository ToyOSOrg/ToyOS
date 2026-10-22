---
status: open
kind: defect
opened: 2026-09-29
---

# The loader never sets the firmware's memory-overwrite request

Owner decision, 2026-10-17: request supported firmware memory wiping after an
unexpected reset, accepting the loss of unsaved crash evidence. This replaces
the earlier decision to preserve RAM for crash records. Do not clear a request
left by another OS to preserve logs, or exempt a diagnostic RAM region from
wiping. Saved diagnostics remain useful; reset-surviving RAM is not a required
source of evidence.

The loader owns firmware access before `ExitBootServices`. Implement the
platform's supported memory-overwrite request and applicable protection against
unauthorized clearing. Unsupported firmware and failed requests must be reported
accurately, not represented as protected. This is reset-attack mitigation, not a
guarantee against every physical memory attack or power-loss scenario.

`loader/src` currently does not handle `MemoryOverwriteRequestControl` under
GUID `e20939be-32d4-41be-a150-897f85d49829`. The existing T14 capture under
Ubuntu `6.8.0-142-generic` found the request value 1 and its lock value 0
(efivarfs bytes `07 00 00 00 01` and `07 00 00 00 00`, respectively). Thus an
inherited request is evidence of a firmware interface, not evidence that ToyOS
sets it or that this firmware wipes RAM on each relevant reset path.

Implementation and firmware behavior remain unverified. Use the governing
firmware specification for request and lock semantics; do not assume setting a
variable proves the wipe occurred.

**Exit:**
- The loader requests wiping through the supported firmware interface, respects
  its lock semantics, and never clears it merely for crash-record retention.
- Host tests cover supported, absent, locked and failed firmware responses.
- Relevant firmware tests check request state; batch a non-secret memory-marker
  reset test on supported benches to verify actual wiping. Record the machine,
  firmware and reset path. Unsupported or unverified paths remain explicit.
- Diagnostics and reset tests accept that unsaved RAM records can disappear;
  no crash-retention requirement disables this mitigation.
