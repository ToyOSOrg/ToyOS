# Services and failure

## The supervisor

- The one program the kernel starts, and the holder of the system capability.
- It starts every program with what that program's manifest row grants
  ([The system manifest](../using/manifest.md)), and creates every service port
  before any server runs, so a client's first connection works whether or not
  its server has started.
- It never dies and stays small: its decisions are pure, host-tested logic,
  and the process only holds handles, spawns and runs its loop.
- It owns the machine's orderly stop, derived from the manifest's
  dependencies. Once it asks the kernel to end the machine, no program but
  the asking thread runs its own code again; the kernel waits for no
  program to finish, so what must finish first is done before the ask.
- It replaces a running service's binary when an authorised
  caller in a login session asks.

The other services are the components in the [Overview](overview.md). Each is
an ordinary process holding only what its row grants.

## Failure

```text
   service crashes ──> parent keeps its endpoint ──> restart (bounded attempts)
                                                      │
                     clients: what was durable survives; open state answers "gone"
```

- **Parents supervise.** A crashed service is restarted by its parent within a
  bounded number of attempts; a feature may be interrupted meanwhile.
- **A service's identity outlives its process.** The parent keeps the endpoint
  while recovery is owed and gives the replacement only its authorised
  capabilities.
- **Recovery restores durable state, not sessions.** File servers recover what
  was on disk, not old opens; network sessions may fail; the memory service
  keeps recoverable swapped contents.
- **Nothing is silently replayed.** An operation with an uncertain outcome is
  reported, and retried only where safe; a write whose acknowledgement was lost
  is never replayed behind the client's back.
- **A restart refused for lack of resources stays visibly pending** and retries
  when resources return, without busy-looping. Recovery state is consistent
  across a crash; memory merely kept alive does not establish that.
- **No restart because time passed.** Stalled progress is reported; manual
  action remains available.
- **A compositor crash may end its session**, never unrelated work.
