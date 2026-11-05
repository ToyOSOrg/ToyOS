# Commands

The commands that embody a piece of the design. Each is an ordinary program
holding only what its manifest row grants; a command that changes the system
asks the service that owns the change, which decides.

| Command | Purpose |
|---|---|
| `launch <app>` | Start an app under its own identity and saved decisions, outliving the shell |
| `pkg` | Install, list and remove packages: verify, ask, unpack, commit |
| `update` | Install a signed image from standard input into the idle slot |
| `swap` | Replace a running service's binary with the one on standard input |
| `logout` | List login sessions; sign one out, gracefully or by force |

The SSH server ships in the image but does not start by default, since it
listens on every interface. It accepts public keys only, and a remote login is
a login session like the desktop's.
