# Kernel

The kernel is small, fast and safe, and does what userland cannot:

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

It holds no filesystem, no storage, input, network, audio or USB driver, no
network protocol, no audio mixing, no login and no naming policy, and parses
nothing one program writes for another. What a parent tells the child it
starts, its arguments and environment, is memory the parent lays out in the
child: the kernel keeps none of it. It knows a process by a short name its
parent gives it, a label that carries no authority. The firmware's AML is
interpreted by a userland ACPI service, which tells the kernel the little it
needs, such as how to power off.

## Interrupts post, threads wait

```text
   device --irq--> handler: acknowledge, post the event, return
                                   |
                                   v
              driver thread (userland) wakes, does the work
```

- The handler posts to whatever the driver waits on; the driver's own thread
  does the work.
- No device wait, spin or poll happens under a spinlock or inside a scheduling
  pass.
- Interrupts are masked no longer than a register access needs.

## One way to wait

Every object that can become ready reports readiness the same way, and a thread
waits for any mix of them through one mechanism. A wait never holds a lock on
the object it waits for.

## One declaration of CPU state

The CPU's control registers come from one declaration, applied and asserted on
every CPU.

## Speculative execution

- One decision per CPU says which speculative-execution vulnerabilities it has
  and what its vendor prescribes against each, from the CPU's identity and
  what it enumerates of itself after its microcode has loaded.
- The vendor's published guidance is the only authority on what a CPU has
  and needs: what the CPU enumerates where the guidance lets enumeration
  decide, the vendor's list of affected processors where it does not, and a
  CPU that list omits is treated as affected and reported as assumed so. How
  far protection between programs, and the kernel's own against a core's
  other thread, goes is ToyOS's policy (Trust).
- Every mitigation the kernel applies is one that decision names for the CPU
  it runs on. None is chosen where it is used.
- Each boot states, for each CPU and vulnerability, whether the CPU is
  unaffected, mitigated and by what, or vulnerable and what is missing.

## Failure

- A broken kernel invariant stops the machine with a report on its panel and
  console ([Logs and diagnostics](diagnostics.md)).
- A device that DMAs outside its grant is recorded and stopped, never a crash.

Programs reach the kernel through its system-call ABI
([The program interface](../using/interface.md)).
