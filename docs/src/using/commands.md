# Commands

The programs a user meets from a shell. Each is an ordinary program that holds
only what its manifest row grants; a command that changes the system asks the
service that owns the change, which decides.

## The shell and the toolbox

| Command | Purpose |
|---|---|
| `shell` | The command interpreter. Built in: `cd`, `export`, `launch` (start an app under its own identity, outliving the shell), `detach`, `exit`, `help` |
| `terminal` | A shell in a window |
| `toybox` | Small utilities under their usual names: `ls`, `cat`, `cp`, `mv`, `rm`, `mkdir`, `pwd`, `echo`, `grep`, `hexdump`, `free`, `ps`, `shutdown`, `reboot`, and ToyOS's own `screen`, `locale`, `stats` and `tone` |

## The system

| Command | Purpose |
|---|---|
| `pkg` | Install, list and remove packages: verify, ask, unpack, commit |
| `update` | Install a signed image from standard input into the idle slot and boot it next |
| `swap` | Replace a running service's binary with the one on standard input |
| `disks` | List disks and partitions; designate and format the data partition |
| `grants` | List, add and revoke the home folders installed packages are granted |
| `logout` | List login sessions; sign one out, gracefully or by force |
| `wifi` | Radio status, scan, join and leave networks |

## Looking inside

| Command | Purpose |
|---|---|
| `inspect` | Ask the kernel and the services what they say about themselves |
| `trace` | Print the kernel's diary |
| `symbolize` | Name the frames of a killed program from its build |
| `host` | Resolve a name to its addresses |

## Remote access

The SSH server is built into the image but not started by default: it listens
on every interface, so a machine that wants remote access starts it
explicitly. It accepts public keys only, and a remote login is a login session
like the desktop.
