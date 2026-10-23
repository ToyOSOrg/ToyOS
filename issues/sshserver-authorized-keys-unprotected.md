---
status: open
kind: defect
opened: 2026-08-06
---

# What sshserver spawns directly holds its keys

`/state/sshserver/host_ed25519` is the machine's SSH private key and
`/state/sshserver/authorized_keys` is the list of who may log in. No row's view
holds another row's `/state` (`Program::view`, `lib/manifest/src/lib.rs`), so a
shell and the programs it starts reach neither. **sshserver's own direct
children still do**: a direct child inherits its parent's view, and sshserver
spawns directly every program an SSH `exec` names that no row declares
(`Connection::start`, `system/sshserver/src/connection.rs`). Such a program,
run by anyone who logs in, can read the first file and rewrite the second, the
second being the one that matters: appending a line to it is a remote login.

Deliberately not worked around here: a daemon-private hiding place would be
obfuscation, and inventing a user model to serve one daemon is the wrong shape
for the decision. Until the exit below, **sshd's trust boundary is the machine,
not the account**: anyone who can run a program over SSH can already be anyone.

The daemon does what it can from where it stands: it is not in any boot config,
it offers public keys only, an `authorized_keys` entry carrying options
authorizes nothing (the options are the restrictions, and honouring the key
without them grants more than the file says), and a host key that exists but
does not parse is refused rather than replaced, because minting over it would
change the identity every client has pinned.

**Exit**: sshserver parents no program: an SSH session's shell and `exec`
start under that session's owner as independent launches, so nothing
sshserver starts holds its view. Stage 3 of
`issues/every-program-sees-only-the-files-it-was-given.md` then keeps the key
list in the login authority's view alone.
