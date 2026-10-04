---
status: open
kind: tooling
opened: 2026-08-03
---

# A staged image went missing from its own lane directory, and nothing explains it

Two tests in one gate failed on an artifact that is not there, in a run where
238 of 240 passed: `usb_flush_optional` with `read the image: No such file or
directory` and `usb_transport_break` with the same `NotFound`. Both pass alone
(8 s and 4 s). Both are a staged disk image missing from the lane directory
that the same test wrote it to.

That leaves the failure without a mechanism. It is worth an hour from whoever
next touches the harness's staging, because "re-run it" stops being an adequate
answer once the failure can be a missing file rather than a slow one — a slow
test reports the content it was going to assert, and this one reports nothing
about the tree at all.
