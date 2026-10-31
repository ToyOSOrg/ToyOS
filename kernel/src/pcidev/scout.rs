//! Measurement scout, not for landing: what holds the T14's package in PC2.
//! `SYS_DEBUG` toggles in a test-actuators kernel, each logged, none undone.
//!
//! - [`power_report`]: every function's PCIe power registers, one line each,
//!   to set beside the bench Linux's `lspci -vvv`.
//! - [`ltr_off`]: one function's LTR Mechanism Enable cleared.
//! - [`display_off`]: the firmware's scan-out stopped, from Intel's Tiger Lake
//!   display reference (IHD-OS-TGL-Vol 12-12.21, "Disable Sequence" step 2;
//!   registers from IHD-OS-TGL-Vol 2c-12.21): stage 1 clears pipe A's planes'
//!   Plane Enable ("plane memory fetches cease"), stage 2 its transcoder's
//!   Transcoder Enable.

use alloc::string::String;
use core::fmt::Write;

use toyos_abi::syscall::SyscallError;
use toyos_pci::{bar, express, pm};

use super::{requester, MACHINE};
use crate::drivers::pci::PciDevice;
use crate::mm::policy::MmioPolicy;

/// PCIe Base 5.0 §7.5.3: the Express capability's link and second device registers.
const LINK_CAPABILITIES: u64 = 0x0C;
const LINK_CONTROL: u64 = 0x10;
const LINK_STATUS: u64 = 0x12;
const DEVICE_CAPABILITIES_2: u64 = 0x24;
/// Device Control 2 bit 10.
const LTR_ENABLE: u16 = 1 << 10;

/// PCIe Base 5.0 §7.6: extended capability ids.
const EXT_LTR: u16 = 0x18;
const EXT_L1SS: u16 = 0x1E;

/// The extended capability `id`'s offset, if the list reaches it.
fn extended(pci: &PciDevice, id: u16) -> Option<u64> {
    let mut at = 0x100u64;
    for _ in 0..64 {
        let header = pci.read_config_u32(at);
        if header == 0 || header == u32::MAX {
            return None;
        }
        if header as u16 == id {
            return Some(at);
        }
        let next = u64::from(header >> 20) & !3;
        if next < 0x100 {
            return None;
        }
        at = next;
    }
    None
}

fn line(pci: &PciDevice) -> String {
    let mut out = String::new();
    let _ = write!(out, "scout: power PCI {:02x}:{:02x}.{} {:04x}:{:04x}", pci.bus, pci.dev, pci.func, pci.vendor_id(), pci.device_id());
    if let Ok(cap) = pci.capability(pm::CAP_ID) {
        let _ = write!(out, " pmcsr={:04x}", cap.read_u16(pm::PMCSR));
    }
    if let Ok(cap) = pci.capability(express::CAP_ID) {
        let _ = write!(
            out,
            " pcie={:04x} lnkcap={:08x} lnkctl={:04x} lnksta={:04x} devcap2={:08x} devctl2={:04x}",
            cap.read_u16(express::CAPABILITIES),
            cap.read_u32(LINK_CAPABILITIES),
            cap.read_u16(LINK_CONTROL),
            cap.read_u16(LINK_STATUS),
            cap.read_u32(DEVICE_CAPABILITIES_2),
            cap.read_u16(express::DEVICE_CONTROL_2),
        );
    }
    if let Some(at) = extended(pci, EXT_L1SS) {
        let _ = write!(
            out,
            " l1sscap={:08x} l1ssctl1={:08x} l1ssctl2={:08x}",
            pci.read_config_u32(at + 4),
            pci.read_config_u32(at + 8),
            pci.read_config_u32(at + 0xC),
        );
    }
    if let Some(at) = extended(pci, EXT_LTR) {
        let _ = write!(out, " ltr_max_snoop={:04x} ltr_max_nosnoop={:04x}", pci.read_config_u16(at + 4), pci.read_config_u16(at + 6));
    }
    out
}

pub fn power_report() -> u64 {
    let functions = MACHINE.lock().functions.clone();
    for pci in &functions {
        log!("{}", line(pci));
    }
    functions.len() as u64
}

pub fn ltr_off(who: u64) -> u64 {
    let Ok(who) = u16::try_from(who) else { return SyscallError::InvalidArgument.to_u64() };
    let Some(pci) = MACHINE.lock().functions.iter().find(|pci| requester(pci) == who).copied() else {
        return SyscallError::NotFound.to_u64();
    };
    let Ok(cap) = pci.capability(express::CAP_ID) else { return SyscallError::NotSupported.to_u64() };
    let before = cap.read_u16(express::DEVICE_CONTROL_2);
    cap.write_u16(express::DEVICE_CONTROL_2, before & !LTR_ENABLE);
    log!("scout: PCI {:02x}:{:02x}.{} devctl2 {before:04x} -> {:04x}", pci.bus, pci.dev, pci.func, cap.read_u16(express::DEVICE_CONTROL_2));
    u64::from(before & LTR_ENABLE != 0)
}

/// Pipe A, its planes 1 to 7 and its transcoder (Vol 2c: PLANE_CTL_n_A at
/// 70180h + (n - 1) * 100h, PLANE_SURF 1Ch past it, TRANS_CONF_A, PIPE_FRMCNT_A).
const PLANE_CTL_1_A: u64 = 0x7_0180;
const PLANE_SURF: u64 = 0x1C;
const PLANE_ENABLE: u32 = 1 << 31;
const TRANS_CONF_A: u64 = 0x7_0008;
const TRANSCODER_ENABLE: u32 = 1 << 31;
const TRANSCODER_STATE: u32 = 1 << 30;
const PIPE_FRMCNT_A: u64 = 0x7_0040;
const TRANS_DDI_FUNC_CTL_A: u64 = 0x6_0400;
/// Read only, for the log: DC states, power wells, CD clock, DBUF.
const STATUS: [(&str, u64); 5] =
    [("dc_state_en", 0x4_5504), ("pwr_well_ctl2", 0x4_5404), ("fuse_status", 0x4_2000), ("cdclk_ctl", 0x4_6000), ("dbuf_ctl_s1", 0x4_5008)];
/// Two frames at 30 Hz, a hang ceiling: the reference's own is "two frame times".
const FRAMES_NANOS: u64 = 70_000_000;

pub fn display_off(stage: u64) -> u64 {
    let Some(gpu) = MACHINE.lock().functions.iter().find(|p| (p.bus, p.dev, p.func) == (0, 2, 0) && p.vendor_id() == 0x8086).copied()
    else {
        return SyscallError::NotFound.to_u64();
    };
    let Ok(base) = gpu.memory_bar(0).map(bar::Memory::address) else { return SyscallError::NotSupported.to_u64() };
    let regs = crate::mm::paging::map_mmio(base, 0x8_0000, MmioPolicy::Uncacheable);
    let mut status = String::new();
    for (name, at) in STATUS {
        let _ = write!(status, " {name}={:08x}", regs.read_u32(at));
    }
    let conf = regs.read_u32(TRANS_CONF_A);
    log!("scout: display stage {stage}: trans_conf_a={conf:08x} trans_ddi_func_ctl_a={:08x}{status}", regs.read_u32(TRANS_DDI_FUNC_CTL_A));
    if conf & TRANSCODER_STATE == 0 {
        return SyscallError::NotFound.to_u64();
    }
    match stage {
        1 => {
            let mut stopped = 0;
            for n in 0..7 {
                let ctl = PLANE_CTL_1_A + n * 0x100;
                let value = regs.read_u32(ctl);
                if value & PLANE_ENABLE != 0 {
                    regs.write_u32(ctl, value & !PLANE_ENABLE);
                    // The write that arms the double-buffered control.
                    regs.write_u32(ctl + PLANE_SURF, regs.read_u32(ctl + PLANE_SURF));
                    log!("scout: plane {} of pipe A was {value:08x}, disabled", n + 1);
                    stopped += 1;
                }
            }
            let frame = regs.read_u32(PIPE_FRMCNT_A);
            let start = crate::clock::nanos_since_boot();
            wait_until_or(start + FRAMES_NANOS, || regs.read_u32(PIPE_FRMCNT_A).wrapping_sub(frame) >= 2);
            log!(
                "scout: pipe A counted {} frames in {} us after its planes' disable",
                regs.read_u32(PIPE_FRMCNT_A).wrapping_sub(frame),
                (crate::clock::nanos_since_boot() - start) / 1000
            );
            stopped
        }
        2 => {
            regs.write_u32(TRANS_CONF_A, conf & !TRANSCODER_ENABLE);
            let start = crate::clock::nanos_since_boot();
            wait_until_or(start + FRAMES_NANOS, || regs.read_u32(TRANS_CONF_A) & TRANSCODER_STATE == 0);
            let after = regs.read_u32(TRANS_CONF_A);
            log!("scout: transcoder A {conf:08x} -> {after:08x} in {} us", (crate::clock::nanos_since_boot() - start) / 1000);
            u64::from(after & TRANSCODER_STATE == 0)
        }
        _ => SyscallError::InvalidArgument.to_u64(),
    }
}

fn wait_until_or(at: u64, done: impl Fn() -> bool) {
    while !done() && crate::clock::nanos_since_boot() < at {
        core::hint::spin_loop();
    }
}
