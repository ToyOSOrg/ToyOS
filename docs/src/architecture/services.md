# Services and failure

## The supervisor

The supervisor is the one program the kernel starts. It reads the signed system
manifest and, from each program's row, builds that program's namespace of
services, its view of files, its device claims and its narrowed system rights,
then spawns it with them. It creates every service port before any server runs.
It never dies and stays small; its decisions are pure, host-tested logic, and
the process itself only holds handles, spawns and runs its loop.

It also owns the machine's orderly stop, derived from the manifest's
dependencies, and the replacement of a running service's binary, which only an
authorised caller in a login session can ask for.

## The services

| Service | Serves |
|---|---|
| File servers | One per storage role: system, data, log, boot and temporary files |
| Block service | Partitions of every disk, to the clients their grants admit |
| Disk and USB drivers | Their controllers' disks and devices |
| Network stack | Sockets, routing and name resolution over the NIC drivers |
| Sound server | Mixing every client's audio onto the output devices |
| Compositor | Windows, the display and the desktop session's input |
| Owner of input | Keyboard and pointer sources from every bus |
| Log keeper | The log files, from every program's log ring |
| ACPI service | The firmware's AML: power button, events, sleep states |
| Memory service | Reclaim and swap |
| File picker | Handing one chosen file to an app |

Each is an ordinary process holding only what its row grants. A client reaches
one only through a connector in its namespace.

## Failure

```text
   service crashes ──> parent keeps its endpoint ──> restart (bounded attempts)
                                                      │
                     clients: what was durable survives; open state answers "gone"
```

- **Parents supervise.** A crashed service is restarted by its parent within a
  bounded number of attempts. A feature may be interrupted while it restarts.
- **A service's identity outlives its process.** The parent keeps the endpoint
  while recovery is owed and gives the replacement only its authorised
  capabilities. A client's next request reaches the new process.
- **Recovery restores durable state, not sessions.** File servers recover what
  was on disk, not old opens; network sessions may fail; the memory service
  keeps recoverable swapped contents. Stable endpoints do not promise to
  restore a client's session.
- **Nothing is silently replayed.** An operation whose outcome is uncertain is
  reported, and retried only where that is safe; a write whose
  acknowledgement was lost is never replayed behind the client's back.
- **A restart refused for lack of resources stays visibly pending** and retries
  when resources return, without busy-looping. Recovery state is consistent
  across a crash; memory merely kept alive does not establish that.
- **No automatic hang restart.** Stalled progress is reported; nothing is
  restarted or ended only because time passed. Manual action remains available.
- **Failure stays contained.** A compositor crash may end its session, never
  unrelated work. A contained device may stay unavailable until reboot. A
  session owner's failure ends at most its own session.
