//! The firmware's boot variables, written for the running system: an entry
//! for an EFI system partition, `BootOrder` with this loader's entry first,
//! `BootNext`, and the entry the firmware would try after this one.
//!
//! **Why the loader and not the kernel.** `SetVariable` is a runtime service,
//! and the kernel never maps the runtime: it would have to call firmware code
//! with its own page tables live, on firmware's stack discipline, with the
//! machine's interrupts and CPUs its own — a surface the kernel does not have
//! and does not want for four variables. The loader runs under boot services,
//! where every variable call is the ordinary one; so the running system writes
//! what it wants into the slot table's request (`toyos_update::slots::Request`)
//! and the next pass writes the variables. What the running system can reach
//! this way is exactly the pure decisions in `toyos_update::entry`: an entry
//! for an ESP the loader found, by its removable-media path, first in the
//! order or next once — never a device path it names.

use alloc::string::String;
use alloc::vec::Vec;
use toyos_update::entry::{self, Partition};
use uefi::prelude::*;
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::media::file::{File, FileAttribute, FileMode};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::media::partition::{GptPartitionType, PartitionInfo};
use uefi::table::runtime::{VariableAttributes, VariableVendor};
use uefi::{cstr16, CStr16, CString16};

/// The head of every line this module writes.
pub const HEAD: &str = "Boot entries:";

/// What every entry this loader writes is called in the firmware's menu.
const DESCRIPTION: &str = "ToyOS";

/// Boot variables are non-volatile and readable by the operating system too
/// (UEFI 2.10 §3.3: `Boot####`, `BootOrder` and `BootNext` are NV, BS, RT).
const ATTRIBUTES: VariableAttributes = VariableAttributes::NON_VOLATILE
    .union(VariableAttributes::BOOTSERVICE_ACCESS)
    .union(VariableAttributes::RUNTIME_ACCESS);

/// The most bytes a load option this loader writes can take: a description,
/// a HARDDRIVE node and a short path.
const OPTION_BYTES: usize = 256;

/// The most entries `BootOrder` is read with: past this a firmware's order is
/// not one this loader rewrites.
const MAX_ORDER: usize = 256;

/// The EFI system partition `guid` names, as a HARDDRIVE node names it, with
/// its removable-media loader checked to be there: `Err` says which of those
/// it is not.
pub fn esp(bs: &BootServices, guid: &[u8; 16]) -> Result<Partition, String> {
    let handle = crate::loaderlog::volume_handle(bs, guid)?;
    let info = crate::rootimage::try_get_protocol::<PartitionInfo>(bs, handle)
        .map_err(|e| alloc::format!("the partition names no partition record ({e:?})"))?;
    let gpt = info.gpt_partition_entry().ok_or("the partition is not a GPT one")?;
    if { gpt.partition_type_guid } != GptPartitionType::EFI_SYSTEM_PARTITION {
        return Err(String::from("the partition is not an EFI system partition"));
    }
    let path = crate::rootimage::try_get_protocol::<DevicePath>(bs, handle)
        .map_err(|e| alloc::format!("the partition's device path ({e:?})"))?;
    let node = path
        .node_iter()
        .filter(|node| node.full_type() == (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE))
        .last()
        .ok_or("the partition's device path carries no HARDDRIVE node")?;
    let hd = <&uefi::proto::device_path::media::HardDrive>::try_from(node)
        .map_err(|e| alloc::format!("the partition's HARDDRIVE node ({e:?})"))?;
    let PartitionSignature::Guid(signature) = hd.partition_signature() else {
        return Err(String::from("the partition's HARDDRIVE node names it by no GUID"));
    };
    if signature.to_bytes() != *guid {
        return Err(String::from("the partition's HARDDRIVE node names another partition"));
    }
    let part = Partition { number: hd.partition_number(), start: hd.partition_start(), size: hd.partition_size(), guid: *guid };
    drop(path);
    drop(info);
    // Not exclusive: this is a look at a volume firmware may be serving to
    // another agent, and EXCLUSIVE would stop that agent to take it.
    let mut fs = crate::rootimage::try_get_protocol::<SimpleFileSystem>(bs, handle)
        .map_err(|e| alloc::format!("its volume would not open ({e:?})"))?;
    let mut root = fs.open_volume().map_err(|e| alloc::format!("it has no volume ({e})"))?;
    let loader = CString16::try_from(crate::arch::REMOVABLE_PATH).expect("the removable path is ASCII");
    root.open(&loader, FileMode::Read, FileAttribute::empty())
        .map_err(|e| alloc::format!("it carries no {} ({e})", crate::arch::REMOVABLE_PATH))?;
    Ok(part)
}

/// Every `Boot####` entry firmware holds, with its option's bytes.
fn entries(rt: &RuntimeServices) -> Result<Vec<(u16, alloc::boxed::Box<[u8]>)>, String> {
    let keys = rt.variable_keys().map_err(|e| alloc::format!("firmware would not list its variables ({e})"))?;
    let mut out = Vec::new();
    for key in keys.iter().filter(|key| key.vendor == VariableVendor::GLOBAL_VARIABLE) {
        let Ok(name) = key.name() else { continue };
        let Some(number) = entry::number(&alloc::format!("{name}")) else { continue };
        match rt.get_variable_boxed(name, &key.vendor) {
            Ok((bytes, _)) => out.push((number, bytes)),
            Err(e) => return Err(alloc::format!("Boot{number:04X} would not read ({e})")),
        }
    }
    Ok(out)
}

/// The entry that boots `part`: the lowest active one already naming it, or
/// one written here at the lowest free number. Its number, and whether it was
/// written.
pub fn entry_for(rt: &RuntimeServices, part: &Partition) -> Result<(u16, bool), String> {
    let held = entries(rt)?;
    if let Some(number) =
        held.iter().filter(|(_, o)| entry::active(o) && entry::names(o, &part.guid)).map(|(n, _)| *n).min()
    {
        return Ok((number, false));
    }
    let used: Vec<u16> = held.iter().map(|(n, _)| *n).collect();
    let number = entry::free(&used).ok_or("every Boot#### number is taken")?;
    let mut option = [0u8; OPTION_BYTES];
    let len = entry::load_option(DESCRIPTION, part, crate::arch::REMOVABLE_PATH, &mut option)
        .map_err(|why| alloc::format!("the entry would not encode: {why}"))?;
    let name = CString16::try_from(alloc::format!("Boot{number:04X}").as_str()).expect("an ASCII name");
    rt.set_variable(&name, &VariableVendor::GLOBAL_VARIABLE, ATTRIBUTES, &option[..len])
        .map_err(|e| alloc::format!("firmware refused Boot{number:04X} ({e})"))?;
    Ok((number, true))
}

/// A `UINT16` variable or array of them, as firmware holds it.
fn words(rt: &RuntimeServices, name: &CStr16) -> Result<Option<Vec<u16>>, String> {
    match rt.get_variable_boxed(name, &VariableVendor::GLOBAL_VARIABLE) {
        Ok((bytes, _)) if bytes.len() % 2 == 0 && bytes.len() / 2 <= MAX_ORDER => {
            Ok(Some(bytes.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect()))
        }
        Ok((bytes, _)) => Err(alloc::format!("{name} holds {} bytes, which is no order this loader reads", bytes.len())),
        Err(e) if e.status() == Status::NOT_FOUND => Ok(None),
        Err(e) => Err(alloc::format!("{name} would not read ({e})")),
    }
}

/// `BootOrder` with `number` first; the order it was and the order it is.
pub fn put_first(rt: &RuntimeServices, number: u16) -> Result<(Vec<u16>, Vec<u16>), String> {
    let was = words(rt, cstr16!("BootOrder"))?.unwrap_or_default();
    let mut now = [0u16; MAX_ORDER + 1];
    let n = entry::first(&was, number, &mut now);
    let bytes: Vec<u8> = now[..n].iter().flat_map(|w| w.to_le_bytes()).collect();
    rt.set_variable(cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE, ATTRIBUTES, &bytes)
        .map_err(|e| alloc::format!("firmware refused BootOrder ({e})"))?;
    Ok((was, now[..n].to_vec()))
}

/// `BootNext` is `number`: the firmware boots that entry at the next reset and
/// deletes the variable as it does (UEFI 2.10 §3.1.2), so once.
pub fn boot_next(rt: &RuntimeServices, number: u16) -> Result<(), String> {
    rt.set_variable(cstr16!("BootNext"), &VariableVendor::GLOBAL_VARIABLE, ATTRIBUTES, &number.to_le_bytes())
        .map_err(|e| alloc::format!("firmware refused BootNext={number:04X} ({e})"))
}

/// The entry the firmware would have tried after the one that booted this
/// pass, skipping an inactive entry and every entry naming `ours` — this
/// loader's own partition, which has just refused every slot it has.
pub fn after_this_one(rt: &RuntimeServices, ours: Option<&[u8; 16]>) -> Result<(u16, Option<u16>), String> {
    let current = words(rt, cstr16!("BootCurrent"))?
        .and_then(|w| w.first().copied())
        .ok_or("firmware names no BootCurrent")?;
    let order = words(rt, cstr16!("BootOrder"))?.ok_or("firmware holds no BootOrder")?;
    let held = entries(rt)?;
    let skip = |number: u16| match held.iter().find(|(n, _)| *n == number) {
        None => true,
        Some((_, option)) => !entry::active(option) || ours.is_some_and(|guid| entry::names(option, guid)),
    };
    Ok((current, entry::after(&order, current, skip)))
}

/// A list of entry numbers as firmware's menu spells them.
pub fn spelled(order: &[u16]) -> String {
    let mut out = String::new();
    for (i, n) in order.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&alloc::format!("{n:04X}"));
    }
    out
}

/// The firmware's boot state as this pass found it — which entry booted it and
/// the order behind that — in one line: what a reader of `loader.log` needs to
/// say which way the machine goes at the next reset.
pub fn state(rt: &RuntimeServices) -> String {
    let current = match words(rt, cstr16!("BootCurrent")) {
        Ok(Some(w)) => w.first().map_or(String::from("none"), |n| alloc::format!("Boot{n:04X}")),
        Ok(None) => String::from("none"),
        Err(why) => why,
    };
    let order = match words(rt, cstr16!("BootOrder")) {
        Ok(Some(order)) => spelled(&order),
        Ok(None) => String::from("none"),
        Err(why) => why,
    };
    alloc::format!("{HEAD} this pass was booted as {current}; BootOrder is {order}")
}
