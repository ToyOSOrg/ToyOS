//! The kernel's console as the host reads it off the virtio port: every byte
//! from the kernel's first record on, and none before it.
//!
//! A firmware whose UEFI console drives that port writes its own lines and the
//! loader's there first, the last of them cut off where boot services end and
//! the kernel's first record written onto its tail. Every one of them is on the
//! 16550 as well, and a loader line is read there. Where the kernel begins is
//! the whole rule, so a firmware that writes nothing on the port reads the same.
//! A terminal is shown that whole, and what is the kernel's in colour
//! ([`Painter`]); the bytes the host reads are never coloured.

use std::borrow::Cow;

use toyos_logstream::{Severity, Shown};

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

/// The console as a terminal shows it: what comes before the kernel's first
/// record as it came, and every line from it on as `toyos_logstream::Shown`
/// draws it — the kernel's records and programs' lines by their heads, and a
/// record's continuation in the severity of the record above it.
///
/// From the kernel's first record on the console carries whole lines only, so
/// a line is held until it ends; before it, only what could still begin
/// [`HEAD`] is.
pub struct Painter {
    begun: bool,
    held: Vec<u8>,
    severity: Severity,
}

impl Default for Painter {
    fn default() -> Self {
        Self { begun: false, held: Vec::new(), severity: Severity::Info }
    }
}

impl Painter {
    /// What of the next `chunk` the terminal is shown now.
    pub fn pass(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        self.held.extend_from_slice(chunk);
        let head = HEAD.as_bytes();
        if !self.begun {
            match self.held.windows(head.len()).position(|w| w == head) {
                Some(at) => {
                    out.extend(self.held.drain(..at));
                    self.begun = true;
                }
                None => {
                    let keep = (1..head.len()).rev().find(|&k| self.held.ends_with(&head[..k])).unwrap_or(0);
                    out.extend(self.held.drain(..self.held.len() - keep));
                    return out;
                }
            }
        }
        while let Some(end) = self.held.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.held.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            let shown =
                toyos_logstream::shown(line).unwrap_or(Shown { head: None, severity: self.severity, text: line });
            self.severity = shown.severity;
            out.extend_from_slice(format!("{shown}\n").as_bytes());
        }
        out
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
         (optimal granularity: not reported), in 22519000 counter ticks\n\
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
        let later = format!("{KERNEL}{{1.002 supervisor}} supervisor: a program's line\n");
        assert_eq!(passed(&later, &[3, 40]), later);
    }

    /// What the terminal is shown of `stream` fed in pieces cut at each of `cuts`.
    fn painted(stream: &str, cuts: &[usize]) -> String {
        let mut painter = Painter::default();
        let mut out = Vec::new();
        let mut from = 0;
        for &to in cuts.iter().chain([stream.len()].iter()) {
            out.extend_from_slice(&painter.pass(&stream.as_bytes()[from..to]));
            from = to;
        }
        String::from_utf8(out).expect("the painter writes UTF-8")
    }

    /// The firmware's bytes reach the terminal as they came, the kernel's
    /// lines as a screen shows them, a continuation in its record's severity —
    /// and the same however the chunks fall.
    #[test]
    fn a_terminal_is_shown_the_firmware_as_it_came_and_the_kernel_in_colour() {
        let alert = "[kernel 0.002 cpu1 alert tid=4] PANIC: panicked at kernel/src/main.rs:1:\n  oops\n";
        let program = "{0.003 warn supervisor} supervisor: a program's line\n";
        let stream = format!("{FIRMWARE}{KERNEL}{alert}{program}");
        let mut want = String::from(FIRMWARE);
        let mut severity = Severity::Info;
        for line in format!("{KERNEL}{alert}{program}").lines() {
            let shown = toyos_logstream::shown(line).unwrap_or(Shown { head: None, severity, text: line });
            severity = shown.severity;
            want.push_str(&format!("{shown}\n"));
        }
        assert!(want.contains("\x1b[91m  oops\x1b[0m\n"), "{want:?}");
        assert_eq!(painted(&stream, &[]), want);
        let every_byte: Vec<usize> = (1..stream.len()).collect();
        assert_eq!(painted(&stream, &every_byte), want);
        // What is held before the kernel is only what could begin its head.
        assert_eq!(painted(&FIRMWARE[..FIRMWARE.len() - 3], &[]), FIRMWARE[..FIRMWARE.len() - 3]);
        assert_eq!(painted("so [ker", &[]), "so ");
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
