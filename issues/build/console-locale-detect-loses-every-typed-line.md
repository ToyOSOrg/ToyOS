---
status: open
kind: defect
opened: 2026-08-31
---

# `console_locale_detect` loses all ten typed lines, with the fix that retired it in the tree

Seen on CI 2026-08-31, pull-request `ci` run 33411831704, job 99553283770
(`guest (1)`), headSha `30918d0e`:

```
FAIL console_locale_detect: 10 typed lines and none of them came back
  FAIL  console_locale_detect  (30s)
  ALONE console_locale_detect: GREEN, and it was alone both times — nothing the
  harness controls differed, so it failed once and passed once. That is a rate
  and not a classification.
```

The shard was otherwise green: `196 passed, 1 failed, 197 total (144.8s)`.

**Not about the branch it appeared on.** `ci-qemu-pin`'s whole delta is
`.github/` and `src/ci.rs` — it repoints the hosted jobs' apt source at a dated
Debian archive so a rolling release cannot move the QEMU version, and it adds
two gates over that. No kernel, userland, or harness byte, and the QEMU the
guest ran is the same 11.1.0 the declaration names.

**What makes this worth an issue rather than a shrug.** `src/redlist.rs` carries
a RETIRED row for this same test. Its retirement reads: typing was the loss —
26 set-1 bytes in one QMP batch against QEMU's 16-byte device queue — and names
the fix that retired it, `shell_type_line` (`7a033450`, 2026-08-26), which
bounds bursts by the queue depth, takes the guest's own echo of the whole line
as the verdict, and retries three times.

That fix is in this tree, and the message above is *its own verdict*. A bounded
burst dropping bytes at a queue boundary explains a mangled line; it does not
explain ten lines out of ten, each retried three times, none echoing back. So
the retired row's mechanism does not account for this sighting, and the
retirement is not evidence against it.

What that leaves is a hypothesis nobody has tested: the loss is upstream of the
typing, in whether the guest was listening at all — a shell not yet at a prompt,
a console that has not yet lent the keyboard, or a boot that reached the marker
wait before the surface existed. All ten lines failing together is the shape of
"nobody was reading", not of "the wire dropped bytes".

Exit: an experiment that discriminates "the bytes were not delivered" from "the
guest was not listening" — the i8042 counter line already distinguishes them
(the retired row read 66 bytes against 72 injected), so a sighting that records
that counter alongside the echo failure decides it. Then fix the cause, and
retire this row against the site that enforces it.
