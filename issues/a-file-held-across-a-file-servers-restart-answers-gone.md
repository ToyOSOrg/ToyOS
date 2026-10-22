---
status: open
kind: defect
opened: 2026-09-27
---

# A file held across a file server's restart answers Gone

No volume records an object id, so nothing tells the held file from another
put at its path since, and a file held across a restart answers `Gone`
(`toyos::files`, so std's `File` and a C descriptor alike); `logd` loses
`/log` for the boot if LOG's server restarts.

Owner decision, 2026-10-17: interrupted handles may report `Gone`; transparent
reopening is not a general first-release requirement. Do not require object
ids and generations solely to restore every held handle. See
`issues/the-supervisor-is-host-tested-and-owns-the-stop.md` for the recovery
policy.

**Exit**: resolve the recorded loss of logging for the rest of the boot through
explicit, protocol-safe reconnection, or report that logging remains unavailable.
Do not silently bind an existing file handle to a replacement at the old path
or replay writes whose outcomes are uncertain. Verify the current logkeeper
behavior before implementation; the paths above predate its rename.
