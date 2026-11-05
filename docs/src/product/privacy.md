# Access, privacy and data

These are the user-visible rules. The mechanisms behind them are
[Processes and authority](../architecture/authority.md) and
[Sessions and permissions](../architecture/sessions.md).

## Permissions

- Programs ask for a right at run time, when they need it, and the answer,
  approval or denial, can be remembered. There is no up-front permission
  list.
- Choosing a file grants that file, not its folder. Remembered access does not
  follow renames. Writable access to a document includes replacing it safely,
  scoped to that file.
- Network use can prompt on first use. Incoming and outgoing access are
  separate per-app choices, local connections included. Listeners and refused
  attempts are visible; there are no per-port approvals. Without an approval
  interface a request fails promptly; explicit commands can grant beforehand.
- A program may pass on a capability it holds without a new prompt, never with
  wider rights. Delegated access shows where it came from. Denying an app
  directly does not cancel access another program delegated to it.
- Removing a permission affects future grants only; stopping the app is a
  separate action.
- A verified update keeps an app's decisions; an unverified replacement does
  not inherit them.
- Clipboard contents reach an app when the user pastes; reading in the
  background needs approval. Screen capture is chosen per session and ends when
  sharing stops. Camera and microphone have separate remembered approvals and
  visible use. Notifications need a remembered approval.

## Storage and encryption

- bcachefs is the native filesystem. Other modern formats are designed for,
  with ext4 and btrfs as the architectural cases, and built when needed.
- Personal storage is shared and encrypted. Any regular user can unlock it after
  a reboot through their own enrolled method; OS permissions separate users.
  Contents and file names are protected; sizes and timestamps visible on the
  raw disk are acceptable.
- Secure passwordless login is preferred where the hardware supports it.
  Passwords are allowed; a passwordless account needs a recovery route. A stored
  recovery key is offered; exporting it needs separate system authority. There
  is no automatic administrator escrow, so losing every recovery method can mean
  losing data.
- The screen lock protects a session while its work continues. Storage stays
  unlocked through sleep and sign-out until shutdown or restart. This is not
  protection against every physical attack on powered memory.
- Apps can keep their own tokens in built-in local credential storage; a chosen
  third-party password manager does not face competing prompts.
- Apps and users share free space, with a reserve for the OS. The system warns
  early and shows what is growing; it never deletes caches by itself.
- Writes are buffered, with explicit durable saving. Native file names are
  case-sensitive. Crash recovery and the format's own integrity protections are
  required.

## Diagnostics

Diagnostics stay on the machine until explicitly exported. Reports are small by
default; memory dumps need an opt-in. Local recovery tools work without AI.
