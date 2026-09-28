//! The kernel's console as the host reads it off the virtio port: every byte
//! from the kernel's first record on, and none before it.
//!
//! A firmware whose UEFI console drives that port writes its own lines and the
//! loader's there first, the last of them cut off where boot services end and
//! the kernel's first record written onto its tail. Every one of them is on the
//! 16550 as well, and a loader line is read there. Where the kernel begins is
//! the whole rule, so a firmware that writes nothing on the port reads the same.

use std::borrow::Cow;

/// What every kernel record's console line opens with: `write_line` in
/// `kernel/src/log/console.rs` tags each record `kernel`, and nothing before
/// the kernel writes it.
pub const HEAD: &str = "[kernel ";

/// A console's bytes as they arrive, withheld until the kernel's first record.
///
/// Of a QEMU process's first boot only: a guest reset does not re-arm it
/// (`issues/build/the-kernel-console-split-does-not-re-arm-across-a-guest-reset.md`).
pub struct KernelConsole {
    /// The withheld tail that could still begin [`HEAD`]; `None` once the
    /// kernel has begun.
    held: Option<Vec<u8>>,
}

impl Default for KernelConsole {
    fn default() -> Self {
        Self { held: Some(Vec::new()) }
    }
}

impl KernelConsole {
    /// What of the next `chunk` is the kernel's.
    pub fn pass<'a>(&mut self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        let Some(held) = &mut self.held else { return Cow::Borrowed(chunk) };
        held.extend_from_slice(chunk);
        let head = HEAD.as_bytes();
        if let Some(at) = held.windows(head.len()).position(|w| w == head) {
            let from = held.split_off(at);
            self.held = None;
            return Cow::Owned(from);
        }
        held.drain(..held.len().saturating_sub(head.len() - 1));
        Cow::Borrowed(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU 11.1.1's own edk2 on the virtio port, as a boot put it there: the
    /// screen clears, `BdsDxe` and the loader, and the loader's last line cut
    /// off by the handoff.
    const FIRMWARE: &str = "\x1b[2J\x1b[01;01H\x1b[=3h\x1b[2J\x1b[01;01HBdsDxe: loading Boot0001 \
         \"UEFI QEMU QEMU USB HARDDRIVE TOYOS0BOOTSTICK1\" from PciRoot(0x0)/Pci(0x1,0x0)/USB(0x0,0x0)\n\
         ToyOS Bootloader 1.0\n\
         ROOT: read into memory at 0x7c894000+0x800000 from LBA 212992+16384, 1048576 bytes a request \
         (optimal granularity: not reported), in 22519000 TSC cycles\n\
         Loader log: the kernel handoff begins, so ";

    const KERNEL: &str = "[kernel 0.000 cpu0 boot] black box: 0x8000000 is this boot's, 16344 bytes \
         for the next boot's loader\n\
         [kernel 0.001 cpu0 boot] pmm: the firmware map calls 4288393216 bytes usable\n";

    /// Everything `stream` passes, fed in pieces cut at each of `cuts`.
    fn passed(stream: &str, cuts: &[usize]) -> String {
        let mut console = KernelConsole::default();
        let mut out = Vec::new();
        let mut from = 0;
        for &to in cuts.iter().chain([stream.len()].iter()) {
            out.extend_from_slice(&console.pass(&stream.as_bytes()[from..to]));
            from = to;
        }
        String::from_utf8(out).expect("the kernel's bytes are UTF-8")
    }

    #[test]
    fn the_fused_line_is_the_kernels_first_record_wherever_the_chunks_fall() {
        let stream = format!("{FIRMWARE}{KERNEL}");
        for cut in 0..=stream.len() {
            assert_eq!(passed(&stream, &[cut]), KERNEL, "cut at byte {cut}");
        }
        let every_byte: Vec<usize> = (1..stream.len()).collect();
        let kernel = passed(&stream, &every_byte);
        let first = kernel.lines().next().expect("a first line");
        assert_eq!(
            crate::bootlog::message(first),
            Some("black box: 0x8000000 is this boot's, 16344 bytes for the next boot's loader")
        );
    }

    #[test]
    fn a_port_the_firmware_left_alone_passes_whole() {
        let later = format!("{KERNEL}{{1.002 init}} init: a program's line\n");
        assert_eq!(passed(&later, &[3, 40]), later);
    }

    #[test]
    fn nothing_before_the_kernel_passes() {
        assert_eq!(passed(FIRMWARE, &[5, 200]), "");
        let mut console = KernelConsole::default();
        assert!(console.pass(FIRMWARE.as_bytes()).is_empty());
        let held = console.held.as_ref().expect("the kernel has not begun").len();
        assert!(held < HEAD.len(), "{held} bytes withheld, more than could begin the head");
    }
}
