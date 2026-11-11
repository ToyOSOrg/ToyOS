# Processes and authority

```text
   process A                         process B
   +-----------------+               +-----------------+
   | 3 -> pipe  (R)  |   transfer    | 7 -> pipe  (R)  |
   | 4 -> conn (RWT) | ------------> |                 |
   +-----------------+   rights      +-----------------+
                         unchanged or narrowed, never widened
```

## Handles are the only authority

- A process holds a table of handles; each names one kernel object and carries
  a set of rights.
- A handle number means something only in its own table. A number copied from
  another process, or counted up from zero, names that process's own slot or
  nothing.
- Every system call states the right it needs on each handle it takes.
- Nothing else grants authority: process and thread ids are identities, object
  ids for diagnostics reach nothing, and a service name means something only
  inside a namespace someone handed you.

## Rights only narrow

- A holder can duplicate a handle with the same or fewer rights, if the handle
  allows duplication, never with more.
- A move, in a message or to a new child, carries the rights unchanged or the
  fewer its mover names, if the handle allows moving.
- Moving on is itself a right: a handle given without it stays with the
  process it was given to.
- Some handles, such as a device claim, cannot be duplicated, so passing one on
  means giving it away. Some, such as an inbox, cannot be moved at all.
- Nothing takes a handle back from a running process: what it was given it
  holds until it gives it up or ends.

## Kernel objects

- A closed set of kinds, among them pipes, connections, service ports,
  namespaces, shared memory, paged memory supplied by a userland pager, events,
  inboxes for asynchronous requests, processes, jobs, device claims and the
  system capability.
- An object crosses a process boundary in exactly two ways: in a message over a
  connection, or endowed to a new child.
- Threads are not objects; nothing outside a process names its threads until a
  debugger or another real caller needs it.

## Communication

- A connection carries bounded, whole messages, each holding bytes and handles
  together, so a message and the authority it passes arrive atomically.
- Notifications coalesce, and polling for readiness does not consume one;
  signalling and consuming each need their own right.
- Pipes remain for real byte streams; bulk data stays in shared memory.
- What a message means belongs to the userland protocol that defines it.

## Creating a process

- A child starts by spawn, never by fork, with an empty handle table.
- It receives exactly one set of endowments, moved out of the parent, all or
  nothing (see [The program interface](../using/interface.md)).
- The parent receives a handle to the child's process, with which it waits for
  the child's end and can stop it.
- Without any handle a process can act only on itself and read machine facts:
  run its threads, manage its address space, read its own record, the clock,
  the CPU count, randomness and overall memory, and create new pipes, ports and
  shared memory, which confer nothing over what already exists.

## The process tree

- Every process has one parent. A parent's end, whether exit, crash or kill,
  ends its whole subtree at once; quitting gracefully is the parent's job.
- A child's end is readiness on its process handle, and says how: an exit code,
  a kill, or a fault of a named class.
- There are no process groups, POSIX sessions, controlling terminals, signals,
  wait-any or calls addressed by process id. The process handle is the only
  control.

## The system capability

- One per machine, held by the supervisor.
- Every privileged right, such as claiming a device, real-time scheduling,
  reading the log, reading counters, listing processes or powering off, is a
  narrowed copy the supervisor hands to the programs its manifest grants.

## Namespaces

- A namespace is an immutable kernel object mapping service names to
  connectors; a narrower one is a new object.
- A process connects to a service only by looking its name up in a namespace it
  holds. There is no global registry.

## Files

Files are reached through directory capabilities, never a global path space
([Files and views](files.md)).
