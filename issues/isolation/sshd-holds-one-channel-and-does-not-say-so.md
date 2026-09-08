---
status: open
kind: defect
opened: 2026-09-08
---

# A second session channel on one sshd connection aliases the first one's input

`userland/sshd`'s session handler keeps one channel and one input sender:
`SshSession::channel` holds the channel a request has not taken yet, and
`SshSession::input` is where `Handler::data` puts everything a client sends.
Neither is keyed on a `ChannelId`, and `channel_open_session` accepts every
open it is given.

So a client that opens two session channels on one connection gets one input
stream. The second `exec` overwrites `input`, and from then on **data the
client sends on channel A reaches the program running on channel B** — and
`channel_eof` on either closes whichever stdin `input` currently holds. The
output direction is unaffected: each program's forwarder task owns its own
channel's write half.

Nothing in the tree does this today. The harness's client
(`tests/ssh-client-host`) opens one channel per connection, and OpenSSH's `ssh`
and `sftp` each open one — measured 2026-09-08, both interoperate with this
daemon over `hostfwd`. A multiplexed OpenSSH client (`ControlMaster`) is the
shape that would reach it, and so is any client that runs a second command on
one connection.

It predates the exec and SFTP work: the daemon had the same two fields when its
only request was a shell. What that work added is a second kind of consumer
(the SFTP request stream), so the two now alias each other as well.

Two answers are open and neither is obviously right:

- **Key the state on `ChannelId`** — a map from channel to input sender, which
  is what the protocol actually describes and what every real server does.
- **Refuse a second channel by name**, `channel_open_session` answering `false`
  once one is in flight. Smaller, and honest about what the daemon serves, but
  it turns a legal client into a refused one.

The first is the right shape; the second is what a daemon that will not
implement the first should do instead of aliasing.
