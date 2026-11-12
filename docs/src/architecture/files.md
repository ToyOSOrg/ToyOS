# Files and views

## A view per program

Every process has its own view of the file tree, built by its parent. A view is
a set of directory capabilities, each a connection to a file server that serves
one subtree with read-only or read-write access. Paths keep working: a path
names something only if it lies inside the caller's view, and the client
library routes it to the directory capability with the longest matching
prefix. An empty view names nothing.

- **A child's view is at most its parent's.** Nothing widens a view after
  spawn. A launch is the exception, and the launched program receives its own
  row's view (see [Sessions and permissions](sessions.md)).
- **Resolution cannot leave the view.** The file server resolves every path
  relative to the directory the connection is bound to, by node identity, so
  `..` at that root, a symbolic link, a rename racing a lookup or a held
  directory moved elsewhere never reach outside it. An absolute link is
  resolved again in the client's own view, so it reaches nothing the client
  does not hold.
- **Sharing is granted, never reached.** A file outside a program's view
  reaches it only through the file picker, which hands over that one file.
- **No special cases.** The boot partition is in the updater's view and nobody
  else's; login data is in the login authority's view alone; `/apps` is
  writable only in the installer's view, and every other program sees it
  read-only.

The working directory is the client's: a spawn states the child's starting
directory, which the child resolves in its own view.

## The tree

```text
/system          read-only and signed: programs, libraries, defaults, shared
                 data (fonts, icons, themes, sounds, backgrounds, licences, docs)
/config          machine settings; a file here overrides the system default
/state/<program> a system service's private persistent data
/apps/<name>     installed packages, one directory each, no layout required
/home/<user>     Desktop Documents Downloads Music Pictures Videos Fonts,
                 and Apps/<name>/ for each app's private data
/boot            boot loader, kernel and boot parameters; the updater's alone
/log             logs
/media/<label>   foreign volumes
/tmp             one per session; each service and app has its own
```

- There are no drive letters and no `/usr`, `/var`, `/opt`, `/dev`, `/proc`
  or `/sys`: devices and processes are capabilities, not files.
- There are no dotfiles or hidden folders in the layout. A third-party program
  that insists on `~/.something` may create one only inside its own app
  folder.
- Names are English and never translated.
- A path means the same file in every view; `/tmp` is the one exception. A
  view hides names and never renames them.
- A program learns a location from environment variables its starter sets,
  such as `HOME`. A location grants nothing; the view decides what is
  reachable. An app's `HOME` is its private folder in the user's home, inside
  which config, data, cache and state are the visible `Config`, `Data`,
  `Cache` and `State`.
- An app finds its own files next to its executable.
- Fonts: the system's under `/system`, a user's in their `Fonts` folder.
  sans-serif and system-ui are Open Sans, monospace is JetBrains Mono, and
  serif stays empty until a licence-clean face is chosen.
- The time zone is a machine setting in `/config`. Keyboard layout and
  language are machine defaults there, which a user may override.

## Files

- An open file is identified by its node, not its path, and lookup is relative
  to a directory. Deleting a name keeps existing handles and mappings working
  until their last release; recreating the name makes a distinct file.
- Reading, writing and stating an open file reach its pages in the kernel
  without asking its server, which a path, a name or an open asks (see
  [Memory](memory.md)).
- Each format owns its on-disk structures, allocation and crash recovery.
  Common operations, authority, file lifetimes, asynchronous I/O and mapping
  integration are shared in userland. An unsupported feature fails clearly;
  there is no forced emulation and no format policy in the kernel.
- Ordinary I/O failure returns an error. A file server's crash invalidates
  that server's open files, which then answer that they are gone; a fresh open
  reaches the restarted server. Unsaved changes may be lost; data already
  confirmed durable stays durable.
- Per-session temporary storage is an ordinary file server holding its
  contents in RAM.
