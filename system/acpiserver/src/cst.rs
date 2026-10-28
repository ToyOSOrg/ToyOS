//! The idle states each processor's `_CST` names (ACPI 6.5 §8.4.1.1), handed
//! to the kernel as the states it may enter as they are
//! (`toyos_abi::acpi::op::IDLE`), and again on the processor's
//! `Notify(…, 0x81)`.
//!
//! **What a state means is ACPI's and its vendor's, and is decided here**; the
//! kernel checks only that its CPU can execute the entry. A state is handed
//! over where nothing is owed around its entry, and withheld by name where
//! something is:
//!
//! - type 1 is C1, which every CPU enters with `hlt` (§8.1.2) and is never
//!   handed over;
//! - an Intel FFH state (302223-007 Table 4: vendor 1, class 2) is `MWAIT`
//!   with Arg0 as its hint, withheld where Arg1 bit 1 asks bus-master
//!   avoidance, which ToyOS does not track; any other FFH encoding has no
//!   document in hand. On AMD from Family 17h `MWAIT` reaches only C1: CPUID
//!   Fn0000_0005_EDX enumerates no deeper sub-state (PPR 55922, 57896), so no
//!   AMD FFH state deeper than C1 exists to support;
//! - a SystemIO state is a byte read of its register, handed over off Intel,
//!   where the core traps it as a C-state request (AMD PPR 55922,
//!   MSRC001_0073); on Intel it is a chipset's P_LVLx read (§8.1.3) no machine
//!   ToyOS runs needs;
//! - a type-3 SystemIO state is handed over only on AMD from Family 17h and
//!   on Hygon. For one OSPM keeps the caches coherent, on a multiprocessor by
//!   flushing them before entry (§8.1.4), which ToyOS does not. AMD's rule for
//!   those processors, which its engineers hold and no AMD document in hand
//!   states, is that their caches stay coherent through it with no flush and
//!   that ARB_DIS is unused. BM_STS, which §8.1.4 makes an optional input to
//!   OSPM's C3 policy and no coherence duty, is not read, and BM_RLD, which
//!   OSPM "can optionally" set for C3, stays as firmware set it: the rule
//!   says nothing of either, and PPR 55901 Vol. 5 says of Family 19h Model
//!   11h that BM_RLD is "no longer needed" and BM_STS "no longer used except
//!   for server CPU". AMD's earlier families have no such rule, and their
//!   BKDGs declare their I/O C-state type 2 (Family 16h's 52740
//!   §2.5.3.2.3). An Intel FFH state of any type is handed over: its entry
//!   is the vendor's native instruction, whose only OSPM duty Intel encodes
//!   is bus-master avoidance (302223-007 Table 4, withheld above), and the
//!   processor flushes what a C-state powers off as it enters it (SDM Vol.
//!   3B §20.19.6.4: data "may have been flushed to memory during the process
//!   of entering the idle state");
//! - a state `_CSD` names for software coordination (Table 8.1, SW_ALL or
//!   SW_ANY) is withheld: ToyOS coordinates no CPU's state with another's.
//!   HW_ALL asks nothing of OSPM.
//!
//! **`_CST` gives no residency, so ToyOS's is a policy**: twice the state's
//! latency, which §8.4.1.1 says is the worst case to enter and exit, so a
//! state is entered only for an idle period it spends at least half of
//! asleep. Metal judges it.
//!
//! **`_LPI` is not read**, though a platform may declare both: OSPM uses it in
//! preference to `_CST` only once it has said in `\_SB._OSC` that it supports
//! processor containers, and never mixes the two (§8.4.3.5, §6.2.11.2), which
//! ToyOS does not say. ACPI defines an x86 state's context-loss flags for
//! Intel alone (§8.4.3.3.5), so an AMD `_LPI` state's 0 there has no defined
//! meaning. AArch64's `_LPI` is `issues/aarch64-cpus-idle-only-in-wfi.md`.

use std::collections::BTreeMap;

use toyos_abi::acpi::{IdleDeclaration, IdleEntry, IdleState, IDLE_STATES};
use toyos_aml::{Kind, Value};

use crate::aml::{kind, Aml};
use crate::cpu::{Cpu, Vendor};
use crate::host::{Declared, Kernel, Stopping, OWN};
use crate::init::Names;
use crate::ledger::Ledger;

/// ToyOS's residency for a `_CST` state, by its latency.
const RESIDENCY_PER_LATENCY: u32 = 2;

/// A processor whose `_CST` is read, and the UID the MADT names it by.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Processor {
    pub path: String,
    pub uid: u32,
    /// It has a `_CSD` beside its `_CST`.
    pub csd: bool,
}

/// Every processor with a `_CST`: each `Processor` object, by its ProcID, and
/// each `ACPI0007` device, by its `_UID` (§5.2.12.2), in walk order.
pub fn processors<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names) -> Vec<Processor> {
    let mut found = Vec::new();
    for named in &names.objects {
        if !names.paths.contains(&format!("{}._CST", named.path)) {
            continue;
        }
        let uid = match named.kind {
            Kind::Processor { id } => Some(u32::from(id)),
            Kind::Device if crate::osc::processor_device(aml, names, &named.path) => {
                match aml.interpreter.evaluate(&mut aml.host, &format!("{}._UID", named.path), &[]) {
                    Ok(Value::Integer(uid)) => u32::try_from(uid).ok(),
                    _ => None,
                }
            }
            _ => continue,
        };
        match uid {
            Some(uid) => found.push(Processor { path: named.path.clone(), uid, csd: names.paths.contains(&format!("{}._CSD", named.path)) }),
            None => println!("{OWN}{} has a _CST and no numeric _UID the MADT could name it by", named.path),
        }
    }
    found
}

/// What the processors' declarations came to.
#[derive(Default)]
pub struct Census {
    pub kept: u64,
    pub no_cpu: u64,
    pub refused: u64,
    /// `_CST` or `_CSD` refused or malformed, by why.
    pub unread: Ledger,
    /// Each state withheld, by why.
    pub withheld: Ledger,
    /// The states each kept declaration held, and how many held them.
    pub shapes: BTreeMap<String, u64>,
}

impl Census {
    pub fn line(&self) -> String {
        let shapes: Vec<String> = self.shapes.iter().map(|(shape, n)| format!("{n} with {shape}")).collect();
        format!(
            "{}{} kept, {} naming no CPU, {} refused by the kernel; kept: {}; withheld: {}; unread: {}",
            acpiserver_api::IDLE_CENSUS,
            self.kept,
            self.no_cpu,
            self.refused,
            if shapes.is_empty() { "none".into() } else { shapes.join(", ") },
            if self.withheld.is_empty() { "none".into() } else { self.withheld.counts() },
            if self.unread.is_empty() { "none".into() } else { self.unread.counts() },
        )
    }
}

/// Read `processor`'s `_CST` and `_CSD` and hand the kernel what of them
/// `cpu` may enter.
pub fn declare<K: Kernel>(aml: &mut Aml<'_, K>, kernel: &K, processor: &Processor, cpu: Cpu, census: &mut Census) -> Result<(), Stopping> {
    let read = |aml: &mut Aml<'_, K>, method: &str| aml.interpreter.evaluate(&mut aml.host, &format!("{}.{method}", processor.path), &[]);
    let states = match read(aml, "_CST").map_err(|why| kind(&why)).and_then(|v| cst(&v)) {
        Ok(states) => states,
        Err(_) if aml.host.stopping => return Err(Stopping),
        Err(why) => {
            census.unread.see(&format!("_CST: {why}"));
            println!("{OWN}{}._CST unread: {why}", processor.path);
            return Ok(());
        }
    };
    let software = if processor.csd {
        match read(aml, "_CSD").map_err(|why| kind(&why)).and_then(|v| csd(&v)) {
            Ok(software) => software,
            Err(_) if aml.host.stopping => return Err(Stopping),
            Err(why) => {
                census.unread.see(&format!("_CSD: {why}"));
                println!("{OWN}{}._CSD unread, so none of its states is handed over: {why}", processor.path);
                return Ok(());
            }
        }
    } else {
        Vec::new()
    };
    let (declaration, withheld) = translate(processor.uid, &states, &software, cpu);
    for why in withheld {
        census.withheld.see(&why);
    }
    match kernel.idle(&declaration)? {
        Declared::Kept => {
            census.kept += 1;
            *census.shapes.entry(shape(&declaration)).or_insert(0) += 1;
        }
        Declared::NoCpu => census.no_cpu += 1,
        Declared::Refused => {
            census.refused += 1;
            println!("{OWN}{}'s idle states were refused by the kernel", processor.path);
        }
    }
    Ok(())
}

fn shape(declaration: &IdleDeclaration) -> String {
    let states: Vec<String> = declaration.states[..declaration.count as usize]
        .iter()
        .map(|s| match IdleEntry::from_raw(s.entry) {
            Some(IdleEntry::Mwait) => format!("MWAIT {:#x} {} µs", s.address, s.latency_us),
            _ => format!("port {:#x} {} µs", s.address, s.latency_us),
        })
        .collect();
    if states.is_empty() { "C1 alone".into() } else { states.join(" + ") }
}

/// One `_CST` state: its Register, type and latency (Table 8.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CState {
    pub register: Register,
    pub kind: u64,
    pub latency: u64,
}

/// A Generic Register descriptor's fields (§6.4.3.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Register {
    pub space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    pub access_size: u8,
    pub address: u64,
}

const FFH: u8 = 0x7F;
const SYSTEM_IO: u8 = 1;

/// The states `_CST` evaluated to, or why it is no `_CST`.
pub fn cst(value: &Value) -> Result<Vec<CState>, String> {
    let Value::Package(elements) = value else { return Err("no Package".into()) };
    let (Some(Value::Integer(count)), states) = (elements.first(), &elements[1.min(elements.len())..]) else {
        return Err("its first element is no Integer".into());
    };
    if *count != states.len() as u64 {
        return Err(format!("its count is {count}, and {} states follow", states.len()));
    }
    states
        .iter()
        .enumerate()
        .map(|(i, state)| match state {
            Value::Package(s) => match &s[..] {
                [Value::Buffer(register), Value::Integer(kind), Value::Integer(latency), Value::Integer(_power)] => {
                    Ok(CState { register: register_of(register).ok_or_else(|| format!("state {i}'s Register is no Generic Register descriptor"))?, kind: *kind, latency: *latency })
                }
                _ => Err(format!("state {i} is no Package of a Buffer and three Integers")),
            },
            _ => Err(format!("state {i} is no Package")),
        })
        .collect()
}

/// A resource template holding one Generic Register descriptor: tag 0x82,
/// length 12 (§6.4.3.7), then the end tag.
fn register_of(bytes: &[u8]) -> Option<Register> {
    if bytes.len() < 15 || bytes[0] != 0x82 || u16::from_le_bytes([bytes[1], bytes[2]]) != 12 {
        return None;
    }
    Some(Register {
        space: bytes[3],
        bit_width: bytes[4],
        bit_offset: bytes[5],
        access_size: bytes[6],
        address: u64::from_le_bytes(bytes[7..15].try_into().ok()?),
    })
}

/// The `_CST` indices `_CSD` names for software coordination (Table 8.3:
/// NumEntries 6, Revision 0, Domain, CoordType, NumProcessors, Index). Index
/// counts the states after `_CST`'s count from 0: §8.4.1.2's example names
/// its C2-type state, the second, Index 1.
pub fn csd(value: &Value) -> Result<Vec<u64>, String> {
    const SW_ALL: u64 = 0xFC;
    const SW_ANY: u64 = 0xFD;
    const HW_ALL: u64 = 0xFE;
    let Value::Package(entries) = value else { return Err("no Package".into()) };
    let mut software = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let Value::Package(fields) = entry else { return Err(format!("entry {i} is no Package")) };
        let [Value::Integer(6), Value::Integer(0), Value::Integer(_), Value::Integer(coord), Value::Integer(_), Value::Integer(index)] = &fields[..] else {
            return Err(format!("entry {i} is not six Integers of revision 0"));
        };
        match *coord {
            SW_ALL | SW_ANY => software.push(*index),
            HW_ALL => {}
            other => return Err(format!("entry {i}'s coordination type {other:#x} is none of Table 8.1's")),
        }
    }
    Ok(software)
}

/// What of `states` the kernel may enter as they are, and why each other was
/// withheld.
pub fn translate(uid: u32, states: &[CState], software: &[u64], cpu: Cpu) -> (IdleDeclaration, Vec<String>) {
    let mut declaration = IdleDeclaration { processor_uid: uid, count: 0, states: [IdleState { address: 0, latency_us: 0, residency_us: 0, entry: 0, reserved: [0; 7] }; IDLE_STATES] };
    let mut withheld = Vec::new();
    for (index, state) in states.iter().enumerate() {
        let entry = match enterable(state, software.contains(&(index as u64)), cpu) {
            Ok(Some(entry)) => entry,
            Ok(None) => continue,
            Err(why) => {
                withheld.push(why);
                continue;
            }
        };
        let Ok(latency_us) = u32::try_from(state.latency) else {
            withheld.push(format!("a latency of {} µs, past a u32", state.latency));
            continue;
        };
        if declaration.count as usize == IDLE_STATES {
            withheld.push(format!("past the {IDLE_STATES} a declaration holds"));
            continue;
        }
        declaration.states[declaration.count as usize] = IdleState {
            address: state.register.address,
            latency_us,
            residency_us: latency_us.saturating_mul(RESIDENCY_PER_LATENCY),
            entry: entry as u8,
            reserved: [0; 7],
        };
        declaration.count += 1;
    }
    (declaration, withheld)
}

/// How `state` is entered, `None` for C1, or why ToyOS may not enter it as it is.
fn enterable(state: &CState, software: bool, cpu: Cpu) -> Result<Option<IdleEntry>, String> {
    let r = state.register;
    match state.kind {
        1 => return Ok(None),
        2 | 3 => {}
        other => return Err(format!("type {other}, which Table 8.2 does not define")),
    }
    if state.latency == 0 {
        return Err("a latency of 0".into());
    }
    if software {
        return Err("coordinated in software by _CSD".into());
    }
    match r.space {
        FFH if (r.bit_width, r.bit_offset) == (1, 2) => {
            if r.address >> 32 != 0 {
                Err("an Intel FFH state whose hint sets reserved bits 63:32".into())
            } else if r.access_size & 1 << 1 != 0 {
                Err("an Intel FFH state asking bus-master avoidance".into())
            } else {
                Ok(Some(IdleEntry::Mwait))
            }
        }
        FFH => Err(format!("FFH vendor {} class {}, an encoding no document in hand defines", r.bit_width, r.bit_offset)),
        SYSTEM_IO if cpu.vendor == Vendor::Intel => Err("a SystemIO state on Intel, a chipset's P_LVLx read".into()),
        SYSTEM_IO if state.kind == 3 && !coherent_through_c3(cpu) => {
            Err("a type-3 SystemIO state, whose caches OSPM keeps coherent (ACPI 6.5 §8.1.4), off AMD Family 17h on and Hygon".into())
        }
        SYSTEM_IO => Ok(Some(IdleEntry::Port)),
        space => Err(format!("a state in space {space:#x}, which no C-state is entered through")),
    }
}

/// Whether `cpu`'s caches stay coherent through a type-3 SystemIO state with
/// no flush and no ARB_DIS: AMD's rule from Family 17h, and Hygon's.
fn coherent_through_c3(cpu: Cpu) -> bool {
    match cpu.vendor {
        Vendor::Amd => cpu.family >= 0x17,
        Vendor::Hygon => true,
        Vendor::Intel | Vendor::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(space: u8, bit_width: u8, bit_offset: u8, access_size: u8, address: u64) -> Register {
        Register { space, bit_width, bit_offset, access_size, address }
    }

    fn state(register: Register, kind: u64, latency: u64) -> CState {
        CState { register, kind, latency }
    }

    const INTEL: Cpu = Cpu { vendor: Vendor::Intel, family: 6 };
    const AMD_17H: Cpu = Cpu { vendor: Vendor::Amd, family: 0x17 };

    /// An Intel `_CST`: the T14's, as its firmware answers ToyOS's processor
    /// `_OSC` claim, decoded from its tables (#875's check outside the tree).
    fn intel_ffh() -> Vec<CState> {
        vec![state(reg(FFH, 1, 2, 1, 0x00), 1, 1), state(reg(FFH, 1, 2, 1, 0x31), 2, 253), state(reg(FFH, 1, 2, 1, 0x60), 3, 1048)]
    }

    /// An AMD `_CST` of C1 in FFH and two trapped port reads, the second type
    /// 3: the AMD bench's (Family 17h).
    fn amd_system_io() -> Vec<CState> {
        vec![state(reg(FFH, 2, 2, 0, 0x00), 1, 1), state(reg(SYSTEM_IO, 8, 0, 1, 0x414), 2, 400), state(reg(SYSTEM_IO, 8, 0, 1, 0x415), 3, 350)]
    }

    fn kept(d: &IdleDeclaration) -> Vec<(u8, u64, u32, u32)> {
        d.states[..d.count as usize].iter().map(|s| (s.entry, s.address, s.latency_us, s.residency_us)).collect()
    }

    #[test]
    fn intel_ffh_states_of_type_2_and_3_are_handed_over_as_mwait_and_c1_is_hlt() {
        let (d, withheld) = translate(3, &intel_ffh(), &[], INTEL);
        assert_eq!(d.processor_uid, 3);
        assert_eq!(kept(&d), [(IdleEntry::Mwait as u8, 0x31, 253, 506), (IdleEntry::Mwait as u8, 0x60, 1048, 2096)]);
        assert!(withheld.is_empty(), "{withheld:?}");
    }

    #[test]
    fn a_type_3_systemio_state_is_handed_over_from_amd_family_17h_and_on_hygon() {
        let both = [(IdleEntry::Port as u8, 0x414, 400, 800), (IdleEntry::Port as u8, 0x415, 350, 700)];
        for family in [0x17, 0x19, 0x1A] {
            let (d, withheld) = translate(0, &amd_system_io(), &[], Cpu { vendor: Vendor::Amd, family });
            assert_eq!((kept(&d), withheld), (both.to_vec(), vec![]), "AMD Family {family:#x}");
        }
        let (d, withheld) = translate(0, &amd_system_io(), &[], Cpu { vendor: Vendor::Hygon, family: 0x18 });
        assert_eq!((kept(&d), withheld), (both.to_vec(), vec![]), "Hygon");
    }

    #[test]
    fn a_type_3_systemio_state_is_withheld_before_amd_family_17h_and_off_amd_and_hygon() {
        let c3 = "a type-3 SystemIO state, whose caches OSPM keeps coherent (ACPI 6.5 §8.1.4), off AMD Family 17h on and Hygon";
        for cpu in [Cpu { vendor: Vendor::Amd, family: 0x16 }, Cpu { vendor: Vendor::Amd, family: 0x15 }, Cpu { vendor: Vendor::Other, family: 0x17 }] {
            let (d, withheld) = translate(0, &amd_system_io(), &[], cpu);
            assert_eq!(kept(&d), [(IdleEntry::Port as u8, 0x414, 400, 800)], "{cpu:?}");
            assert_eq!(withheld, [c3], "{cpu:?}");
        }
        let (d, withheld) = translate(0, &amd_system_io(), &[], INTEL);
        assert_eq!(d.count, 0);
        assert_eq!(withheld, ["a SystemIO state on Intel, a chipset's P_LVLx read"; 2]);
    }

    #[test]
    fn each_state_ospm_owes_work_around_is_withheld_by_name() {
        let withheld = |s: CState, software: bool, cpu: Cpu| {
            let (d, why) = translate(0, &[s], if software { &[0] } else { &[] }, cpu);
            assert_eq!(d.count, 0);
            why
        };
        assert_eq!(withheld(state(reg(FFH, 1, 2, 0b11, 0x31), 2, 253), false, INTEL), ["an Intel FFH state asking bus-master avoidance"]);
        assert_eq!(withheld(state(reg(FFH, 2, 2, 0, 0x10), 2, 50), false, AMD_17H), ["FFH vendor 2 class 2, an encoding no document in hand defines"]);
        assert_eq!(withheld(state(reg(FFH, 1, 1, 0, 0x1816), 2, 50), false, INTEL), ["FFH vendor 1 class 1, an encoding no document in hand defines"]);
        assert_eq!(withheld(state(reg(SYSTEM_IO, 8, 0, 0, 0x1816), 2, 253), false, INTEL), ["a SystemIO state on Intel, a chipset's P_LVLx read"]);
        assert_eq!(withheld(state(reg(SYSTEM_IO, 8, 0, 1, 0x415), 3, 350), true, AMD_17H), ["coordinated in software by _CSD"]);
        assert_eq!(withheld(state(reg(FFH, 1, 2, 1, 0x31), 4, 253), false, INTEL), ["type 4, which Table 8.2 does not define"]);
        assert_eq!(withheld(state(reg(FFH, 1, 2, 1, 0x31), 2, 0), false, INTEL), ["a latency of 0"]);
        assert_eq!(withheld(state(reg(FFH, 1, 2, 1, 1 << 32), 2, 10), false, INTEL), ["an Intel FFH state whose hint sets reserved bits 63:32"]);
        assert_eq!(withheld(state(reg(0, 8, 0, 1, 0x1000), 2, 10), false, AMD_17H), ["a state in space 0x0, which no C-state is entered through"]);
    }

    #[test]
    fn more_states_than_a_declaration_holds_are_withheld_past_the_last() {
        let many: Vec<CState> = (0..10).map(|i| state(reg(FFH, 1, 2, 1, 0x10 + i), 2, 10 + i)).collect();
        let (d, withheld) = translate(0, &many, &[], INTEL);
        assert_eq!(d.count as usize, IDLE_STATES);
        assert_eq!(withheld.len(), 2);
    }

    fn register_buffer(r: Register) -> Value {
        let mut b = vec![0x82, 12, 0, r.space, r.bit_width, r.bit_offset, r.access_size];
        b.extend(r.address.to_le_bytes());
        b.extend([0x79, 0x00]);
        Value::Buffer(b)
    }

    #[test]
    fn a_cst_package_decodes_and_a_malformed_one_is_refused() {
        let s = |r, kind, latency| Value::Package(vec![register_buffer(r), Value::Integer(kind), Value::Integer(latency), Value::Integer(0)]);
        let good = Value::Package(vec![Value::Integer(2), s(reg(FFH, 1, 2, 1, 0), 1, 1), s(reg(FFH, 1, 2, 1, 0x31), 2, 253)]);
        assert_eq!(cst(&good), Ok(vec![state(reg(FFH, 1, 2, 1, 0), 1, 1), state(reg(FFH, 1, 2, 1, 0x31), 2, 253)]));
        let miscounted = Value::Package(vec![Value::Integer(3), s(reg(FFH, 1, 2, 1, 0), 1, 1)]);
        assert_eq!(cst(&miscounted), Err("its count is 3, and 1 states follow".into()));
        let unregistered = Value::Package(vec![Value::Integer(1), Value::Package(vec![Value::Buffer(vec![0x86, 9, 0]), Value::Integer(2), Value::Integer(1), Value::Integer(0)])]);
        assert_eq!(cst(&unregistered), Err("state 0's Register is no Generic Register descriptor".into()));
        assert_eq!(cst(&Value::Integer(0)), Err("no Package".into()));
    }

    #[test]
    fn csd_names_software_coordinated_states_and_hw_all_none() {
        let entry = |coord, index| Value::Package([6, 0, 0, coord, 2, index].map(Value::Integer).to_vec());
        // The AMD bench's: HW_ALL over each SMT pair, for C1.
        assert_eq!(csd(&Value::Package(vec![entry(0xFE, 0)])), Ok(vec![]));
        assert_eq!(csd(&Value::Package(vec![entry(0xFD, 1), entry(0xFC, 2)])), Ok(vec![1, 2]));
        assert!(csd(&Value::Package(vec![entry(0xFB, 1)])).is_err());
        assert!(csd(&Value::Package(vec![Value::Package([5, 0, 0, 0xFE, 2].map(Value::Integer).to_vec())])).is_err());
    }
}
