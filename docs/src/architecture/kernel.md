# Kernel

The kernel is small, fast and safe. It does what userland cannot:

- schedules threads and accounts their CPU time and memory to jobs;
- protects memory and supplies generic paging and sharing mechanisms;
- holds kernel objects and the handles that name them, and moves handles
  between processes;
- routes interrupts to the processes that wait for them;
- confines each device's DMA to the memory granted to it, through the IOMMU;
- reads the firmware's static tables for CPUs, interrupt controllers, timers
  and IOMMUs, and enumerates the device functions it hands out as claims;
- loads CPU microcode and applies the CPU's control registers;
- keeps its own log record ring and its panic console.

The firmware's AML is interpreted by a userland ACPI service, which tells the
kernel the little it needs, such as how to power the machine off. The kernel
holds no filesystem, no driver for a storage, input, network, audio or USB
device, no network protocol, no audio mixing, no login and no policy about
names. It parses nothing that userland writes for another userland program.

## Interrupts post, threads wait

An interrupt handler acknowledges its device and posts the event to whatever
the driver waits on; the driver's own thread does the work (see
[Overview](overview.md) for the kernel's rule on threads and work). No device
wait, spin or poll happens while a spinlock is held or inside a scheduling
pass, and interrupts are masked no longer than a register access needs.

```text
   device --irq--> handler: acknowledge, post the event, return
                                   |
                                   v
              driver thread (userland) wakes, does the work
```

## One way to wait

Every kind of kernel object that can become ready reports readiness the same
way, and a thread waits for any mix of them through one mechanism. A wait never
holds a lock on the object it waits for.

## One declaration of CPU state

The CPU's control registers come from one declaration, applied and asserted
on every CPU, so no CPU runs with a setting another lacks.

## Failure

A broken kernel invariant stops the machine with a crash report on its panel
and in a record that survives the reset (see
[Logs and diagnostics](diagnostics.md)). A device that DMAs outside its grant
is recorded and stopped, never a crash.

## The boundary with programs

The kernel's interface with programs is its system-call ABI, which is
unstable and may change for a cleaner design. It is only that boundary:
protocols between programs define their own messages and errors and never use
the kernel's. POSIX compatibility lives in libc, never in the kernel's native
interface. See [The program interface](../using/interface.md).
