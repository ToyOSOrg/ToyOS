# The program interface

A ToyOS program sees three layers, and an ordinary program uses only the top
one.

```text
   Rust std  /  libc            portable programs: unchanged source
   ─────────────────────────
   the native SDK               ToyOS-specific programs: typed handles,
                                services, views, devices, windows
   ─────────────────────────
   the kernel ABI               system calls on handles; unstable
```

## The kernel ABI

Every system call is one of these families:

| Family | What it does |
|---|---|
| Own execution | Exit, threads, futex waits, sleep, thread names |
| Own address space | Reserve, commit, map, protect and release memory |
| Handles | Duplicate with narrower rights, close, move |
| Waiting | Watch any set of objects for readiness, and wait |
| Processes | Spawn from memory ranges the parent lays out, with endowments; wait for and stop a child |
| Communication | Pipes, service ports, connections carrying bytes and handles, inboxes |
| Shared and paged memory | Create, map and lend memory; supply pages as a pager; read, write and state what a pager holds |
| Devices | Claim through the system capability, map device memory, grant DMA, wait for interrupts |
| Machine facts | Clock, CPU count, randomness, overall memory |
| Privileged | Listing processes, reading the log and counters, real-time scheduling, power: each a right on the system capability |

The ABI changes freely for a cleaner design; programs rebuild against the
matching SDK. Its errors are the kernel's, and are translated at the SDK's
boundary: protocols between programs define their own.

## The native SDK

The SDK is the Rust interface to ToyOS: typed handles and rights, endowments a
program was started with, namespaces and service connections, framed messages,
pollers, shared memory, files through the program's view, spawning and
launching, device claims for drivers, logging, and windows for programs that
draw. Each service's protocol is a library of its own that owns its message
types and errors.

## Rust's standard library

std behaves as on any platform: files resolve through the program's view,
threads, thread-locals, unwinding, backtraces and dynamic loading work,
networking reaches the network stack and processes start by spawn. A command's
working directory and environment are passed to the child, and the child's
`HOME` and temporary directory are those its starter set.

## libc

libc provides C and C++ programs with standard C and the POSIX subset described
in [Applications and portability](../architecture/applications.md). It is a
userland library over the SDK.

## How a program starts

```text
   endowments ──> standard streams (descriptors 0, 1, 2 and those named)
              ──> a namespace of services it may connect to
              ──> directory capabilities: its view
              ──> anything else its row or its parent gave it

   its memory ──> its program, in the ranges its parent laid out
              ──> a startup block below it: its arguments and its
                  environment (HOME, the starting directory, ...)
```

A parent reads the child's program and lays out the child's memory. What it
tells the child goes in the startup block, which the kernel copies as it
copies the program and never reads; the SDK, std and libc read it where it
lies. A child whose parent wrote none starts with no argument and an empty
environment.

A program holds nothing it was not given. It asks for more only by connecting
to a service in its namespace, whose server decides.
