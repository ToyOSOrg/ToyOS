# Overview

```text
  applications          editor, browser, games, tools ...
  ------------------------------------------------------------------
  SDK and runtimes      Rust std, the native SDK, libc   (in each process)
  ------------------------------------------------------------------
  session owners        one per login session: launch, lifetime, sign-out
  desktop               windows, input, trusted interaction
  resource services     files, disks, network, audio, display, memory, logs
  drivers               one process per device function, behind the IOMMU
  supervisor            starts every program, holds the system's authority
  ==================================================================
  kernel                threads, memory protection, capabilities, IPC,
                        interrupt routing, DMA isolation
  loader                every firmware call, before the handover
```

## Who does what

| Component | Responsibility |
|---|---|
| Kernel | Scheduling, memory protection, IPC, capabilities, interrupt routing, DMA isolation |
| Supervisor | Starts system services and session owners from the system manifest; holds and narrows the system's authority |
| Session owner | One login session's process lifetime and independent app launch |
| Desktop | Windows, input, trusted interaction and system controls |
| Permission service | Saved decisions, runtime consent and authorising fresh grants |
| Authentication | Verifying users, unlocking storage, protected credentials and passkeys |
| Package management | Verifying and installing immutable versions; app identity |
| Resource services | Files, disks, network and audio, and enforcing access to them |
| Memory service | Reclaim policy, pressure reports and encrypted swap |
| Diagnostics | Local inspection, failure evidence and authorised recovery |
| Drivers | Operating a device, its firmware, queues and device-specific recovery |
| SDK and runtimes | Typed native interfaces; ordinary Rust behaviour; C compatibility |
| Applications | Workflows and private state under granted authority |

A responsibility is not necessarily its own process. Filesystem formats and
names, service lookup, network protocols, audio mixing, login and consent all
stay out of the kernel.

## Rules every boundary follows

- **Userland first.** The kernel owns only what userland cannot. It creates no
  thread except each CPU's idle loop. Kernel work is bounded, runs on the thread
  or interrupt that caused it, and is charged to it. Long-running work that no
  one owns belongs in a userland service.
- **Authority is explicit.** A process receives capabilities only through
  authorised creation or transfer. Names, paths and process ids grant nothing.
  Rights can be narrowed and never widened by a holder.
- **A server never blocks on a client.** Accepting a connection and receiving
  its first message are separate events; a message is taken whole without
  waiting; a reply that cannot be sent at once drops that client by name. A
  client that stalls harms only itself.
- **Bulk data travels in shared memory**, with authority and lifetime still
  explicit. Routine data access stays off central control services' hot
  paths.
- **Core machinery is ToyOS's own.** The scheduler, cryptography and
  allocators are written for ToyOS and held to security, correctness and
  measured performance evidence. This is not a ban on dependencies or a demand
  for novel algorithms.
- **Recovery has a bounded path.** Reclaim, writeback, device I/O and service
  recovery never depend on the memory or the service being recovered.
- **Waits are honest.** When a wait's failure timeout expires, the operation
  returns an error where its meaning allows one; otherwise the stall is
  reported and state is kept. Elapsed time alone never terminates or restarts
  anything.
