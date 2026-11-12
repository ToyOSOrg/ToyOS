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
  ids for diagnostics reach nothing, and a name means something only in the
  process whose parent wrote it.

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
  shared memory, paged memory supplied by a userland pager, events,
  inboxes for asynchronous requests, processes, jobs, device claims and the
  system capability.
- An object crosses a process boundary in exactly two ways: in a message over a
  connection, or endowed to a new child.
- Threads are not objects; nothing outside a process names its threads until a
  debugger or another real caller needs it.

## Communication

- A connection carries bounded, whole messages, each holding bytes and handles
  together, so a message and the authority it passes arrive atomically.
- A received handle arrives with the kind of object it names, as the kernel
  read it when it delivered the handle. What the sender's protocol says the
  handle is remains the sender's word: a receiver takes it as a kind only where
  the kernel reported that kind, and refuses any other, including a kind it
  does not know.
- Notifications coalesce, and polling for readiness does not consume one;
  signalling and consuming each need their own right.
- Pipes remain for real byte streams; bulk data stays in shared memory.
- What a message means belongs to the userland protocol that defines it.

## Creating a process

- A child starts by spawn, never by fork. Its table starts with its handle to
  itself, its notice, then exactly one list of endowments moved out of the
  parent, all or nothing, in the order the spawn listed them
  (see [The program interface](../using/interface.md)).
- The kernel keeps no name for any handle. What a parent calls each endowment
  is bytes it writes into the child's memory, and the child's SDK checks them
  once before it looks one up.
- The parent receives a handle to the child's process, with which it waits for
  the child's end and can stop it.
- Without any handle a process can act only on itself and read machine facts:
  run its threads, manage its address space, read its own record, the clock,
  the CPU count, randomness and overall memory, and create new pipes, ports and
  shared memory, which confer nothing over what already exists.

## The process tree

- Every process has one parent. A parent's end, whether exit, crash or kill,
  ends its whole subtree at once.
- A holder of a child's process handle may ask the child and everything
  below it to quit, with a reason: an interrupt, a hang-up or a terminate. The
  kernel carries the request to every process below. A process that listens
  for such requests receives it as an event. One that never listened, or has
  not yet taken the last request, is ended instead, and its subtree with it;
  no process in between can shield what is below it.
- The kernel never asks on its own: whether and when to ask is the holder's
  decision.
- A child's end is readiness on its process handle, and says how: an exit code,
  a kill, or a fault of a named class.
- There are no process groups, POSIX sessions, controlling terminals, signals,
  wait-any or calls addressed by process id. The process handle is the only
  control.

## The system capability

- One per machine, held by the supervisor, which the kernel starts holding it
  and the images it boots from at handles the interface states, since no
  parent names them.
- Every privileged right, such as claiming a device, real-time scheduling,
  reading the log, reading counters, listing processes or powering off, is a
  narrowed copy the supervisor hands to the programs its manifest grants.

## Services

- A service is a port. Its server holds the acceptor; a client holds a
  connector and connects through it, which takes the right to write on the
  connector. A service one program provides to a launch, such as a terminal's
  surface, reaches the launched program only where its row receives that
  service.
- A process finds a service only among the connectors its parent gave it,
  by the name its parent wrote. There is no global registry and no connect by
  name.
- A helper a program spawns is given a copy of every service connector its
  parent holds, unless the spawn names fewer; a launch is given its row's.
  What a process gives itself, as the supervisor does for its own files, no
  child inherits.

## Files

Files are reached through directory capabilities, never a global path space
([Files and views](files.md)).
