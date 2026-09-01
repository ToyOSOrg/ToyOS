---
status: open
kind: track
opened: 2026-09-01
---

# Nothing outside this tree has ever spoken SSH to sshd

`issues/isolation/sshd-accept-path-unexercised.md`: sshd defines its
authentication callbacks and enables public-key authentication only, and no
independent client has ever driven accept, auth, session start and close against
it. A daemon that once accepted every credential is not one to certify against
its own parser.

**What to build.** A host-side protocol client built **from source**, from an
implementation this project did not write, with its source pinned and declared in
`NOTICE` like every other third-party file. It drives a disposable key through
accept, auth, one command, and close. The oracle is the wire capture plus the
exit status.

**The one thing that would void it.** A client that shares protocol code with
sshd reproduces sshd's bugs and agrees with itself. No shared parsing, no shared
framing, no shared crypto glue — if the two implementations agree it must be
because the specification says so.

**And no committed host binary.** The tree's dependency rule is Rust and QEMU
only, and a checked-in SSH binary would be a fifth standing failure beside the
four already declared. Source, built here, or it does not land as the gate.
