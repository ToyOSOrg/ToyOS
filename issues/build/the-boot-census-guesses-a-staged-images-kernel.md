---
status: open
kind: tooling
opened: 2026-10-01
---

# The boot census guesses a staged image's kernel

The suite's closing line, `<n> guests, <m> of them not the shipping kernel`,
counts a boot as not the shipping kernel when `qemu::kernel_of` says so
(`tests/common/qemu.rs`). A boot handed a staged `BootOptions::boot_image`
builds nothing, and `kernel_of` answers from the boot's own options instead of
from the image, so two staged boots are counted as the other kernel:

- an image staged with valued parameters only is the shipping kernel
  (`build_boot_image_carrying`), and is counted as the test kernel, because
  `kernel_params` is not empty;
- an image staged by `build_test_kernel_image` is the test kernel with nothing
  armed (`origin::after_records`), and is counted as the shipping kernel.

The image records its parameters and not its kernel build, so the boot cannot
ask it.

**Exit**: a staged image carries the kernel build it was made with, and
`kernel_of` reads that for a staged boot.
