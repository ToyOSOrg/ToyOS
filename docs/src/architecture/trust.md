# Trust

```text
   trusted                         isolated
   -------------------------       ------------------------------------
   CPU, microcode                  every program, including system ones
   firmware: UEFI, AML, SMM        users and sessions
   the signed image, with its      the network, file contents
   pinned toolchain and crates     devices: DMA only into granted memory
```

ToyOS trusts only what it cannot avoid trusting:

- the CPU and chipset vendor, with its signed microcode;
- the platform firmware, which already controls the whole machine;
- the signed OS image, with its pinned toolchain and crates.

Everything else is isolated, including external and hot-plugged devices, whose
DMA stays behind the IOMMU.

## Privileges beyond a device claim

- A privilege beyond an ordinary device claim, such as a reset line or a
  firmware register window, is granted only by the signed image's system
  manifest, to a program that image ships.
- A package installed later never receives one, whoever signed it.

## Machines without an IOMMU

- The same driver runs with and without an IOMMU; there is never a second
  driver.
- Where the hardware genuinely has no isolation, a driver the signed image
  ships may claim a device and no other may, and the machine states plainly
  that it runs without device isolation.
- External DMA-capable devices always need isolation.
- Unknown isolation, unsupported isolation hardware and a failed setup are
  errors, not evidence that isolation is absent. There is no machine-specific
  exception image.

## External ports and firmware

- A device behind an external port is never claimed by a userland driver.
- The IOMMU does not stop a driver reflashing its own device's firmware.
- Device firmware is loaded by its owning driver through its IOMMU domain. CPU
  microcode loading is privileged kernel work. Neither payload need be embedded
  in the kernel binary.

## Checks inside the trusted base

- A check there is a bug fence: kept where it is cheap and catches ToyOS's own
  mistakes, such as a write landing in RAM, the IOMMU, an interrupt controller
  or another driver's device.
- It never withholds a feature for lack of a vendor document, and ToyOS does
  not reimplement firmware to second-guess it.
