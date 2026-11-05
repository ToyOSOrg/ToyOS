# Processes and authority

## Handles are the only authority

A process holds a table of handles. Each handle names one kernel object and
carries a set of rights. A handle number means something only in its own
process's table: a number copied from another process, or counted up from zero,
names that process's own slot or nothing. Every system call states the right
it needs on the handle it takes.

Nothing else grants authority. Process and thread ids are identities only; no
call acts on a process because its id was named. Object ids for diagnostics are
never a way to reach an object. A service name means something only inside a
namespace someone handed you.

```text
   process A                         process B
   +-----------------+               +-----------------+
   | 3 -> pipe  (R)  |   transfer    | 7 -> pipe  (R)  |
   | 4 -> conn (RWT) | ------------> |                 |
   +-----------------+   rights      +-----------------+
                         unchanged or narrowed, never widened
```

## Rights only narrow

A holder can duplicate a handle with the same or fewer rights, if the handle
allows duplication, and never with more. A move carries the rights unchanged.
Some handles cannot be duplicated at all, such as a device claim, so the only
way to pass one on is to give it away.

## Kernel objects

The kernel's objects are a closed set of kinds, among them pipes, connections
between a client and a service, service ports, namespaces, shared memory,
paged memory supplied by a userland pager, events, inboxes for asynchronous
requests, processes, jobs, device claims and the system capability. An object
crosses a process boundary in exactly two ways: sent in a message over a
connection, or endowed to a new child. Threads are not objects: nothing outside
a process names one of its threads, until a debugger or another real caller
needs it.

## Creating a process

A child starts by spawn, never by fork. It starts with an empty handle table
and receives exactly one set of endowments, which move out of the parent:
its standard streams, any other descriptors, the namespace of services it may
connect to, the directories it may see and anything else the parent chooses.
The spawn is all-or-nothing. The parent receives a handle to the child's
process, with which it can wait for the child's end and stop it.

What every process can do without any handle is limited to itself and to facts
about the machine: run and end its own threads, manage its own address space,
read its own record, read the clock, the CPU count, randomness and overall
memory figures, and create new pipes, ports and shared memory, which confer
nothing over anything that already exists. Listing other processes, reading
the machine's log, powering it off and real-time scheduling each need a right
on the system capability.

## The process tree

Every process has one parent. A parent's end, whether exit, crash or kill, ends
its whole subtree at once; quitting gracefully is the parent's job before it
exits, never the kernel's. A child's end is readiness on its process handle,
and the end says how: an exit code, a kill, or a fault of a named class.

The kernel has no process groups, POSIX sessions, controlling terminals,
signals, wait-any or calls addressed by process id. A process handle is the
only control. libc builds the parts of POSIX that programs need from these
events, and refuses the rest rather than pretending to succeed.

## Communication

A connection carries bounded, whole messages, each holding bytes and handles
together, so a message and the authority it passes arrive atomically.
Notifications coalesce, and polling for readiness does not consume one;
signalling and consuming each need their own right. Pipes remain for real byte
streams, and bulk data stays in shared memory. What a message means belongs to
the userland protocol that defines it, never to the kernel.

## The system capability

The kernel starts exactly one program, the supervisor, and mints the machine's
one system capability for it. Every other privileged right, such as claiming
a device, real-time scheduling, reading the log or listing processes, is a
narrowed copy that the supervisor hands to the programs whose manifest rows
grant it.

## Namespaces

A namespace is an immutable kernel object mapping service names to connectors.
A narrower one is a new object built from an existing one. A process connects
to a service only by looking its name up in a namespace it holds; there is no
global registry to fall back to. Every port exists before any server runs, so a
client's first connection succeeds whether or not its server has started.

## Files are inside the model

Files are reached through directory capabilities, never through a global path
space. See [Files and views](files.md).
