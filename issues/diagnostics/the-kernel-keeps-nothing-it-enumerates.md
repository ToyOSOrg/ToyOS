---
status: open
kind: track
opened: 2026-08-05
---

# The kernel keeps nothing it enumerates, so nothing can be asked what the machine is

`pci::enumerate` returns into a local in `kernel_main` and is dropped. The same
goes for NVMe Identify strings, USB INQUIRY strings, the GPT table and mount
metadata. There is no `hw` and no `disk`, because there is nothing for them to
read. Retention is the bulk of this work and it is blocked on nothing.

On top of retention, in dependency order:

1. **Retention plus one query syscall and a topic decoder**, with `hw` and
   `disk` as its first two consumers.
2. **A diagnostic port on `/bin/init`**, answering a program name with a
   `Process` handle narrowed to `Rights::READ`. `SYS_PROCESS_STATS` takes a
   `Process` handle and a handle is the whole of the right, so nothing hands a
   diagnostic tool a way to sample a daemon: `/bin/init` holds the only `Process`
   handles for what `[boot] start` names and the only `SysCap` carrying
   `Rights::MANAGE`, which is what `SYS_PROCESS_OPEN` takes. So "where is
   soundd's / the compositor's / netd's time going?" is unanswerable from a
   shell, and `audio_idle_suspend` name-matches `SYS_SYSINFO` entries out of a
   byte buffer to sample a running daemon twice. Nothing in the kernel has to
   change for it — the addressing half is done, and this is the policy half.
3. **A status-query message in the SDK**, reserved out of the message-id space,
   answered by the compositor first.
4. **A cumulative soundd snapshot** published under a seqlock, and an `audio`
   tool that reads it. Today's stats struct is windowed and reset wholesale in
   four places, so nothing outside the mix thread can read a total.
5. **netd interface, link and counter reporting**, and a `net` tool — the name
   is taken by an HTTP GET demo that should be renamed `http` first, which is
   the owner's call.
6. **Master gain, and `audio volume`** — see
   `issues/audio/hda-has-no-jack-detection-volume-or-keys.md`, which needs the
   same thing.
7. **Adopting a disk from inside the guest**, so designation stops being a
   host-side whole-device stamp minted by the build system. Blocked on the
   owner, on retention (for witness minting) and on the bcachefs
   untrusted-input residuals.

One residual is nearly free: a log-follow tool is a manifest row and about 30
lines, because `LogTail` already exists in the SDK.

**Syscall numbers: the three this work once reserved are all taken, and the
obvious fix is wrong too.** 97 and 98 are the device register read/write pair,
99 is the endowments call. Re-allocating "from 128 up" breaks a compile-time
assert, because 128 is the syscall-profile bin count and 127 is its overflow
bucket. **The first clean numbers are 116, 117, 118** — 113 and 115 are held
reservations, 114 is spent on log reading, and 96 and 107 are retired and never
reused.

The log half of this work landed differently and better, and the difference is
worth knowing before anything is rebuilt on the old design: reading is
record-shaped rather than byte-shaped, it is authority (`Rights::LOG` on a
`SysCap`, not something any process may call), and userland output never enters
the kernel ring at all — which deletes the span and origin tracking this once
needed, because the feedback loop it existed to prevent is now unrepresentable.
