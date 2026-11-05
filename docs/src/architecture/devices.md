# Devices and drivers

Every driver is a userland process. The kernel hands out only what the IOMMU
can tell apart, PCI functions, ISA port and interrupt rows and platform
devices, and confines each to the memory granted to it.

```text
   kernel: enumerate functions ──> claim (no copy, only a move)
                                     │
   supervisor: match the manifest ───┘──> spawn the driver with its claim
                                                │
   driver process ── DMA only into its grants ──┘
       │
       └─ serves a client protocol: block, input, network, audio, display
```

## Claims

A device claim is a handle that cannot be duplicated, so giving it to a driver
is the only way to pass it on, and the giver provably no longer holds it. Each
call that drives a device presents the claim, and the kernel checks the device
class as well as the handle. The kernel's interrupt handler posts to the
driver's wait; the driver's thread does the work.

A driver reaches only the memory its grants map: the IOMMU domain is the gate,
not the descriptor the driver writes. A device address outside a grant is a
recorded DMA fault and the device is stopped, never a kernel crash. Device
reuse waits until the old holder's DMA is contained and cleaned up.

## The supervisor brokers devices

The system manifest says which program drives which kind of device. The
supervisor is the persistent waiter: a function present at boot and one
arriving later are the same event, answered by the same spawn. The device set
is a level-triggered snapshot the supervisor compares, not an event stream.

- **One driver process per kernel-handed function**: a disk driver per NVMe
  controller, a NIC driver per function, a USB service per USB controller.
- **One shared service per client-facing function**: file servers, the network
  stack, the owner of input, the compositor, the sound server.
- A driver reaches its consumer as a client of the consumer's port; it never
  publishes a name for others to look up.
- Device-provided names never confer authority. Discovery happens in userland.

## Removal and return

Removing a device invalidates its claim, which then answers that it is gone. A
returning device is a fresh arrival with a new handle. Device identity, such as
a serial number or a partition GUID, is the business of the driver and the file
server, never the kernel's. A contained device that failed may stay unavailable
until reboot.

## Buses

A bus service owns everything behind its controller. The USB service, one per
USB controller, keeps device classes such as HID and mass storage inside its own
process and speaks the client protocols directly: input to the owner of input,
disks to the block service. The supervisor never sees a USB device. Short USB
glitches stay inside the USB service.

PCIe hot-plug uses the native mechanism only.

## Distinct devices, distinct owners

Distinct devices have distinct owners, even when they run instances of one
driver. A driver exposes device functions to separately authorised services
rather than probing for roles. A transport is trusted with the data it carries
without inheriting every consumer's rights.

## Graphics

GPU support reuses maintained vendor and upstream components through ToyOS
adapters, with adaptations kept separate and divergence small, so fixes stay
practical to adopt. Hardware-specific logic stays in userland. Rendering is on
the CPU; display drivers handle presentation, panel power and resume.

## Input

Input translation belongs in userland, per surface. Keyboard and pointer
sources from every bus reach the owner of input, which merges them. A dead
kernel takes no input; there is no emergency input path.
