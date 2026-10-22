---
status: open
kind: track
opened: 2026-08-20
---

# The power broker: authority with a human in the loop

Opened from the owner's question about Linux's interactive shutdown
permission, the day `SYS_SHUTDOWN` gained `Rights::POWER`. Linux answers
"the person at the keyboard said so" with three glued-on systems (a
privileged service, a policy engine, an authentication agent) because its
base model only knows identities. ToyOS's capability model IS the mechanism,
so the native shape is smaller:

1. **Programs request, the broker decides** — a port/connection like every
   other daemon protocol in the tree, under the server-never-blocks
   doctrine.
2. **The confirmation rides the trusted UI path.** The broker asks the
   compositor to present it, and the tree's own architecture is what makes
   that mean something: the compositor owns the panel and the kernel
   delivers key transitions per surface, so no ordinary program can draw a
   fake dialog or fake the click on a real one. "A human physically present
   confirmed" is the single-user machine's honest equivalent of Linux's
   password prompt.
3. **Inhibitors**: a program may register "unsaved work" with the broker;
   the broker delays, or names the holdouts in the dialog. Registration is
   a connection, so a crashed registrant releases its inhibit by the same
   teardown that releases everything else.

## Whole-machine sleep

Owner decision, 2026-10-17: the first release targets ordinary sleep with the
session retained in powered RAM. Defer hibernation and its encrypted session
image and authenticated-resume design. Ordinary sleep retains sensitive memory;
it does not cryptographically lock personal storage. This is separate from
idle CPU core sleep. Safe device suspend/resume and platform sleep support
remain implementation work.

## Automatic sleep

Owner decision, 2026-10-18: lid-close sleep behaviour is configurable,
including whether an app's keep-awake request may prevent it. The default and
setting scope remain to define; neither precedence is mandated as the sole
behaviour. This does not change the separate idle-sleep request rules below.

Owner decision, 2026-10-16: applications may temporarily prevent automatic
whole-machine sleep while doing work without separate per-app approval or a
prompt for every request. Show the requesting application and its stated
reason, and let the user override the request. Tie each request to its
connection lifetime so disconnect releases it. The request does not grant
`POWER`, prevent idle CPU cores from sleeping, or authorize control over
shutdown or reboot. The broker protocol, override lifetime and whole-machine
suspend/resume implementation remain design work.

Unstaffed until the owner opens it; sequenced naturally with the userland/
product era. What must not happen meanwhile is the accident this track
exists to prevent: `POWER` spreading to more manifest rows.

Its recorded weakness: the i8042's holder resets the machine without it
(`issues/the-i8042s-holder-holds-the-machines-reset-line.md`).
