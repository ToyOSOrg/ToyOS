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

- A device claim cannot be duplicated, so giving it to a driver is the only
  way to pass it on, and the giver provably no longer holds it.
- Each call that drives a device presents the claim, and the kernel checks the
  device class as well as the handle.
- A driver reaches only the memory its grants map: the IOMMU domain is the
  gate, not the descriptor the driver writes. A device address outside a grant
  is a recorded DMA fault and the device is stopped.
- A device is reused only once the old holder's DMA is contained and cleaned
  up. A contained device that failed may stay unavailable until reboot.

## The supervisor brokers devices

- The system manifest says which program drives which kind of device.
- The supervisor is the persistent waiter: a function present at boot and one
  arriving later are the same event, answered by the same spawn. The device set
  is a level-triggered snapshot it compares, not an event stream.
- **One driver process per kernel-handed function**: a disk driver per NVMe
  controller, a NIC driver per function, a USB service per USB controller.
- **One shared service per client-facing function**: file servers, the network
  stack, the owner of input, the compositor, the sound server.
- A driver reaches its consumer as a client of the consumer's port; it never
  publishes a name for others to look up.
- Discovery happens in userland, and device-provided names confer no
  authority.

## Removal and return

- Removing a device invalidates its claim, which then answers that it is gone.
- A returning device is a fresh arrival with a new handle.
- Device identity, such as a serial number or a partition GUID, belongs to the
  driver and the file server, never the kernel.

## Buses

- A bus service owns everything behind its controller. The USB service keeps
  device classes such as HID and mass storage inside its process and speaks
  the client protocols directly: input to the owner of input, disks to the
  block service. The supervisor never sees a USB device, and short USB glitches
  stay inside the USB service.
- PCIe hot-plug uses the native mechanism only.

## Distinct devices, distinct owners

- Distinct devices have distinct owners, even when they run one driver's code.
- A driver exposes device functions to separately authorised services rather
  than probing for roles.
- A transport is trusted with the data it carries without inheriting every
  consumer's rights.

## Displays

- One driver process per GPU function brings its outputs to the compositor,
  the way an audio driver brings outputs to the sound server.
- Intel's display driver is ToyOS's own Rust, written from Intel's published
  documentation. AMD's adapts AMD's maintained display code, by the owner's
  ruling for that device.
- The display driver registers a panic surface with the kernel.

## Input

- Input translation belongs in userland, per surface.
- Keyboard and pointer sources from every bus reach the owner of input, which
  merges them.
- A dead kernel takes no input; there is no emergency input path.
