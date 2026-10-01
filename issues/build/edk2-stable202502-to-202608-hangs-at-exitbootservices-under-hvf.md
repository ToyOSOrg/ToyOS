---
status: open
kind: tooling
opened: 2026-10-01
---

# edk2 stable202502 to stable202608 hangs at ExitBootServices under HVF

Under QEMU 11.1.1's HVF a `virt` guest reads `ID_AA64PFR0_EL1.GIC` as 0,
though its GICv3's system registers answer. `hvf_arch_init_vcpu`
(`target/arm/hvf/hvf.c:1481`) writes that register once, setting `GIC` only if
`env->gicv3state` is set, and it runs while `machvirt_init` realizes the CPUs
(`hw/arm/virt.c:3140`), before `create_gic` (`:3158`) makes the GIC that sets
it. QEMU master is the same. TCG computes the field on each read
(`id_aa64pfr0_read`, `target/arm/helper.c`) and reads 1.

From edk2-stable202502 (`8edd5fd6d3`, `eaa60a6b10`, `e663b79f74`) to
stable202608, ArmVirtQemu's `ArmGicDxe` runs its GICv3 driver only where that
field is nonzero, and its GICv2 driver otherwise. The DT's `arm,gic-v3` node
never sets the GICv2 CPU interface's base, which stays at its 0 default
(`ArmVirtQemu.dsc`). At `ExitBootServices`, `GicV2ExitBootServicesEvent`
acknowledges interrupts until `GICC_IAR` reads 1020 to 1023. At 0x0 + 0xc the
guest reads the firmware's own flash, `0xffffffff`, so the loop never ends.

Seen at `8d74557fa` with Debian's 2026.05-2, in the orchestrator's runs
`aarch64fw-r2-d1-pinned` and `aarch64fw-r2-d2-pinned-no-vgic`.
`virt_early_panic` and `virt_early_fault`, this host's only HVF guests, stop
after the loader's last line. QMP then reads EL1h, PC `0xbf5e0a80` (the `ret`
after `MmioRead32`'s load, `X0 = 0xffffffff`) and `0xbf5dfbbc` a second later.
The 64 words dumped from 0x80 below the PC occur once in that build's
`ArmGicDxe.efi`, at file offset 0x1b3c: the loop above. With
`kernel-irqchip=off` (QEMU's own GICv3) the loop is the same. Every TCG `virt`
guest boots that build green.

`virt` therefore boots Debian's 2024.11-5, whose `ArmGicDxe` reads no
`ID_AA64PFR0_EL1` and takes the GIC's revision from the DT. That build leaves
`EfiLoaderData` executable (`PcdDxeNxMemoryProtectionPolicy` `0x7fd1`, Debian's
revert patch). It also runs EL2 without setting `HCR_EL2.E2H`. So neither
reason `enter_kernel` (`bootloader/src/arch/aarch64.rs`) turns the MMU off for
reaches any guest. Owner: the orchestrator.

## Exit condition

`virt`'s pin moves to a build that carries edk2's `aefdbf91f4` and
`377a890d80`, where the DT chooses between split GICv2 and GICv3 drivers
again, or to any build under a QEMU whose HVF sets `ID_AA64PFR0_EL1.GIC` for
an attached GICv3. It is shown by `virt_early_panic` and `virt_early_fault`
green under HVF on that pin, and `enter_kernel`'s revert red there under TCG.
