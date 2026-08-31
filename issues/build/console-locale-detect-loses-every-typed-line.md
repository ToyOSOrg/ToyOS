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

## 2026-08-31, a second sighting under the same call site

`desktop_locale_detect` failed with the same sentence on pull-request `ci` run
33426887418, job 99613902394 (`guest (9)`), headSha
`28be5a85ca268647c42537539b5ebb0c0e24d990`:

```
FAIL desktop_locale_detect: 10 typed lines and none of them came back
  FAIL  desktop_locale_detect  (31s)
  ALONE desktop_locale_detect: GREEN, and it was alone both times
```

`12 passed, 1 failed, 13 total (257.1s)`. Both names reach that sentence
through one site and one string: `shell_answers` types `echo surface-up-zqjxk`
and `shell_echoes` reports `{TRIES} typed lines and none of them came back`
after ten attempts. The two differ only in which surface owner is behind the
shell — `/bin/console` for one, `/bin/terminal` under the compositor for the
other — and that is downstream of where the bytes are lost.

## The unacknowledged burst, confirmed in the code

`shell_type_once` (`tests/toyos.rs:6196`) bounds each batch and then sends every
batch of a line back to back inside one `QmpInput` scope, with no guest-side
wait anywhere in it:

```rust
let mut input = qemu::QmpInput::open(qemu.qmp_socket());
for burst in ps2_bursts(line) {
    input.type_burst(&burst);
}
input.keys(&[("ret", true), ("ret", false)]);
```

`console_type_line` (`tests/toyos.rs:6107`) does the opposite on the same
`ps2_bursts` split: it opens the socket per burst and waits for the panel to
echo that burst before the next one goes out. Only the windowed path kept the
unpaced form.

Derived from `scancode_bytes` (`tests/common/qemu.rs:3425`) and the map `qcode`
holds beside it, not measured: the 21 characters of `echo surface-up-zqjxk` are
all unshifted, so two set-1 bytes each — 42 bytes, which `ps2_bursts` splits
into three bursts of 16, 16 and 10 — and Enter is a fourth command of 2 more.
44 bytes against the 16 the device holds, with nothing between them but a QMP
round trip. A QMP reply proves QEMU's main loop ran; it does not prove any vCPU
read port 0x60, which is the only thing that empties the queue.

**Enter is the byte the arithmetic condemns.** It is sent last, so it is behind
all 42, and `shell_type_once`'s verdict is the shell's echo of the whole line —
which reaches the console only when a newline flushes the surface owner's
line-buffered stdout, as `shell_type_line`'s own doc comment states. A line
whose Enter was dropped therefore produces no echo at all rather than a mangled
one, and the next attempt types into a shell whose line buffer still holds the
last attempt's fragment. That is the shape of ten of ten with nothing coming
back, and it is what the retired row's byte-level truncation could not explain.

Both captures are consistent with it. The console sighting's failure body — what
the guest said during the final attempt — is **empty**. The desktop sighting's
carries two `compositor: frames=` lines and nothing from the shell, so the guest
was scheduled and compositing while its shell said nothing.

**Still not discriminated, and this is what the exit above asks for.** Consistent
is not confirmed: "the bytes were dropped at the queue" and "the guest was not
reading" produce the same empty body. The i8042 drain counter separates them and
neither sighting recorded it — the shard-wide census in both jobs reads
`i8042 40` across six reporting guests, which is an aggregate over other boots
and settles nothing about either. The measurement needs a guest run.
