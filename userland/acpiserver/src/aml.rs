//! Everything only the machine's AML can answer: its definition blocks
//! loaded into one namespace ([`toyos_aml`]), and what this server asks of it.
//!
//! [`load`] fetches the DSDT and every SSDT through the kernel
//! ([`crate::tables`]), loads each in the order the firmware lists them, and
//! says one line a table: loaded, or refused and why. **A refused SSDT is
//! said loudly and the load goes on** (the owner's ruling: "Go on, say it
//! loudly"). **A refused DSDT leaves no namespace**: no SSDT is loaded onto
//! nothing, and the server serves the power button as it does without AML.
//! A machine that is stopping ends the load in one line, and that is no
//! refusal.
//!
//! Every read of a table's bytes the kernel refused is counted by the
//! kernel's name for it and the memory type, in one line, whether or not the
//! load reached that table.
//!
//! Then `\_S5` is evaluated (ACPI 6.5 §7.4.2, "\_Sx (System States)") and
//! its `SLP_TYPa` handed to the kernel, which powers the machine off with it
//! and has no other source of it. **A server that handed none over leaves
//! the kernel what an earlier holder of the claim supplied, and no power-off
//! where none did**, whatever kept it — tables unread, a DSDT refused,
//! an `\_S5` that is no package of two integers, a value the register does
//! not hold — and that is said once, loudly ([`NO_S5_HANDED`]); the server
//! goes on serving. **A namespace whose DSDT loaded is kept** ([`Aml`]),
//! with the host that answered its load, for what the server evaluates in it
//! after: the devices it serves ([`crate::devices`]), and, where the power
//! button is a control method device, each embedded-controller query's method.
//!
//! A table's line says its place, whether it is the DSDT or an SSDT, and a
//! refusal by its kind; what the firmware chose — a name, an offset, an
//! address, a bridge's registers — is on a line under [`OWN`].

use std::time::Instant;

use toyos_acpi::TableError;
use toyos_aml::{Error, Host, Interpreter, Value};

use crate::host::{Firmware, Kernel, Refusal, Stopping, OWN};
use crate::tables::Tables;

/// A machine's namespace, and the host every evaluation in it is answered by.
pub struct Aml<'k, K> {
    pub interpreter: Interpreter,
    pub host: Firmware<'k, K>,
}

/// What became of the machine's definition blocks: the load's verdict, which
/// its lines say. Whoever evaluates a method after the load asks `blocks`
/// whether there is a namespace.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// Each block in the order it was loaded, the DSDT first; `Err` is the
    /// kind of its refusal.
    pub blocks: Vec<Result<(), String>>,
    /// `\_S5`'s `SLP_TYPa` and `SLP_TYPb`.
    pub s5: Option<(u64, u64)>,
    /// The kernel kept that `SLP_TYPa`: this machine has a power-off.
    pub handed: bool,
}

/// What a refused evaluation or load is called in a line anyone may quote.
pub fn kind(why: &Error) -> String {
    match why {
        Error::Malformed { why, .. } => format!("malformed AML: {why}"),
        Error::NotFound(_) => "a name that does not resolve".into(),
        Error::Exists(_) => "a name defined twice".into(),
        Error::Type(why) => format!("a type its operator refuses: {why}"),
        Error::Rule(why) | Error::Table(why) | Error::Bound(why) => (*why).into(),
        Error::Fatal { .. } => "the firmware executed Fatal".into(),
        Error::Unsupported(why) => format!("not carried by this interpreter: {why}"),
        Error::Host(why) => format!("denied by this server: {why}"),
        Error::Bridge { .. } => BRIDGE.into(),
    }
}

/// The refusal the interpreter makes of a bridge's own registers.
const BRIDGE: &str = "a bridge above a PCI_Config region answered no bus below it";

/// What a table refused before any of it ran is called; `refused` is why the
/// kernel read none of the range a [`TableError::Unmapped`] names.
fn unread(why: &TableError, refused: impl Fn(u64) -> Option<Refusal>) -> String {
    match why {
        TableError::BadRsdp => "the RSDP does not check".into(),
        TableError::NoXsdt => "the RSDP names no XSDT".into(),
        TableError::Absent => "nothing names it, or what is named is another table".into(),
        TableError::Length { .. } => "it declares a length no table has".into(),
        TableError::Checksum => "its bytes do not sum to zero".into(),
        TableError::Unmapped { at, .. } => match refused(*at) {
            Some(refusal) => format!("its bytes could not be read: {refusal}"),
            None => "its bytes are at no address a table has".into(),
        },
    }
}

const STOPPING: &str = "acpiserver: the machine is stopping, so the tables' load ends here";

/// What is said, at error severity, of a machine whose `\_S5` the kernel was
/// not handed.
pub const NO_S5_HANDED: &str =
    "acpiserver: no \\_S5 was handed to the kernel, which powers off only on one an ACPI server supplied and refuses a shutdown without one";

/// Load the machine's definition blocks from the RSDP at `rsdp`, and
/// evaluate `\_S5`; the namespace, where its DSDT loaded, is kept.
pub fn load<K: Kernel>(kernel: &K, rsdp: u64) -> (Loaded, Option<Aml<'_, K>>) {
    let mut host = Firmware::new(kernel);
    let mut interpreter = Interpreter::new();
    let loaded = load_into(kernel, &mut interpreter, &mut host, rsdp);
    let kept = loaded.blocks.first().is_some_and(|dsdt| dsdt.is_ok()) && !host.stopping;
    (loaded, kept.then_some(Aml { interpreter, host }))
}

fn load_into<K: Kernel>(kernel: &K, interpreter: &mut Interpreter, host: &mut Firmware<'_, K>, rsdp: u64) -> Loaded {
    let began = Instant::now();
    let mut loaded = Loaded::default();
    let tables = Tables::new(kernel);
    let blocks: Vec<_> = match toyos_acpi::definition_blocks(&tables, rsdp) {
        Ok(blocks) => blocks.collect(),
        Err(_) if tables.stopping.get() => {
            println!("{STOPPING}");
            return loaded;
        }
        Err(why) => {
            println!(
                "acpiserver: no namespace: the XSDT was not read ({}{}), so no table is; the power button is served, and nothing of this machine's AML",
                unread(&why, |at| tables.refused(at)),
                tables.last_refused().map_or(String::new(), |refusal| format!("; the last read refused was {refusal}"))
            );
            println!("{OWN}that was {why:x?}");
            toyos::error!("{NO_S5_HANDED}");
            return loaded;
        }
    };
    if tables.stopping.get() {
        println!("{STOPPING}");
        return loaded;
    }

    let count = blocks.len();
    for (place, block) in blocks.iter().enumerate() {
        let name = if place == 0 { "DSDT" } else { "SSDT" };
        let place = place + 1;
        let done = match block {
            Ok(table) => interpreter.load(host, table).map_err(|why| {
                println!("{OWN}table {place} was refused {why:x?}");
                kind(&why)
            }),
            Err(why) => {
                println!("{OWN}table {place} was not read: {why:x?}");
                Err(unread(why, |at| tables.refused(at)))
            }
        };
        if host.stopping {
            println!("{STOPPING}");
            return loaded;
        }
        match &done {
            Ok(()) => println!("acpiserver: table {place} of {count} ({name}) loaded"),
            Err(kind) => println!("acpiserver: table {place} of {count} ({name}) refused: {kind}"),
        }
        loaded.blocks.push(done);
        if place == 1 && loaded.blocks[0].is_err() {
            println!(
                "acpiserver: no namespace: the DSDT was refused, so the {} SSDT(s) after it are not loaded; the power button is served, and nothing of this machine's AML",
                count - 1
            );
            break;
        }
    }

    println!(
        "acpiserver: {} of {count} tables loaded in {} ms",
        loaded.blocks.iter().filter(|block| block.is_ok()).count(),
        began.elapsed().as_millis()
    );
    println!(
        "acpiserver: the tables' bytes took {} reads of firmware memory, in pages: {}",
        tables.reads.get(),
        tables.pages.borrow().by_type()
    );
    let refusals = tables.refusals();
    if !refusals.is_empty() {
        println!("acpiserver: reads of the tables' bytes the kernel refused: {}", refusals.counts());
    }
    println!(
        "acpiserver: the tables' AML read SystemMemory {} times, SystemIO {} and PCI_Config {}, its memory in pages: {}; took the Global Lock {} times, {} of them from the firmware; and ran Notify {} times",
        host.reads[0],
        host.reads[1],
        host.reads[2],
        host.pages.by_type(),
        host.takes,
        host.contended,
        host.notifies
    );

    if loaded.blocks.first().is_some_and(|dsdt| dsdt.is_ok()) {
        match s5(interpreter, host) {
            Ok((a, b)) => {
                println!("acpiserver: \\_S5 evaluated: SLP_TYPa={a} SLP_TYPb={b}");
                loaded.s5 = Some((a, b));
                match kernel.s5(a) {
                    Ok(true) => {
                        println!("acpiserver: \\_S5 handed to the kernel: SLP_TYPa={a}");
                        loaded.handed = true;
                    }
                    Ok(false) => println!("acpiserver: \\_S5 refused: the kernel keeps no SLP_TYPa wider than the register's three bits"),
                    Err(Stopping) => {
                        println!("{STOPPING}");
                        return loaded;
                    }
                }
            }
            Err(_) if host.stopping => {
                println!("{STOPPING}");
                return loaded;
            }
            Err(kind) => println!("acpiserver: \\_S5 refused: {kind}"),
        }
    }
    if !loaded.handed {
        toyos::error!("{NO_S5_HANDED}");
    }
    if !host.refused.is_empty() {
        println!("acpiserver: refused so far, of accesses: {}", host.refused.counts());
    }
    loaded
}

/// `\_S5`'s first two elements: what `SLP_TYPa` and `SLP_TYPb` are written
/// for soft-off.
fn s5(interpreter: &mut Interpreter, host: &mut dyn Host) -> Result<(u64, u64), String> {
    match interpreter.evaluate(host, "\\_S5", &[]) {
        Ok(Value::Package(elements)) => match elements.as_slice() {
            [Value::Integer(a), Value::Integer(b), ..] => Ok((*a, *b)),
            _ => Err("a package that does not begin with two integers".into()),
        },
        Ok(_) => Err("no package".into()),
        Err(why) => {
            println!("{OWN}\\_S5 was refused {why:x?}");
            Err(kind(&why))
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::host::tests::Scripted;
    use crate::host::{Take, HELD};

    fn load_only<K: Kernel>(kernel: &K, rsdp: u64) -> Loaded {
        load(kernel, rsdp).0
    }

    /// QEMU's own tables where its guest held them
    /// (`toyos-acpi/fixtures/qemu-11.1.1/SOURCE`), as ACPI reclaim memory.
    const RSDP: u64 = 0x7fb7_e014;
    const DSDT: u64 = 0x7fb7_a000;
    const QEMU: &[(u64, &[u8])] = &[
        (RSDP, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/rsdp.bin")),
        (0x7fb7_d0e8, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/xsdt.bin")),
        (0x7fb7_9000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/facp.bin")),
        (0x7fb7_8000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/apic.bin")),
        (0x7fb7_7000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/hpet.bin")),
        (0x7fb7_6000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/mcfg.bin")),
        (0x7fb7_5000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/dmar.bin")),
        (0x7fb7_4000, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/waet.bin")),
        (DSDT, include_bytes!("../../../toyos-acpi/fixtures/qemu-11.1.1/dsdt.bin")),
    ];

    fn machine(tables: &[(u64, &[u8])]) -> Scripted {
        Scripted { memory: tables.iter().map(|&(at, bytes)| (at, 9, bytes.to_vec())).collect(), ..Default::default() }
    }

    /// What every guest boot does, against the kernel's stand-in: QEMU's
    /// DSDT is fetched, loads asking the machine for nothing, and its `\_S5`
    /// is the `SLP_TYPa=0` the kernel logged on the boot the tables are of;
    /// its namespace names no embedded controller, and no control-method
    /// button where one is looked for.
    #[test]
    fn qemus_tables_load_and_s5_is_what_its_kernel_decoded() {
        let kernel = machine(QEMU);
        let (loaded, kept) = load(&kernel, RSDP);
        assert_eq!(loaded, Loaded { blocks: vec![Ok(())], s5: Some((0, 0)), handed: true });
        let Aml { mut interpreter, mut host } = kept.expect("QEMU's DSDT loads");
        for control_method in [false, true] {
            assert_eq!(crate::devices::find(&mut interpreter, &mut host, crate::devices::tests::GPE0, control_method), crate::devices::Found::default());
        }
        assert!(kernel.asked.borrow().iter().all(|access| access.write == 0 && access.space == 0));
        assert!(!kernel.held.get());
        assert_eq!(kernel.handed.get(), Some(0));
    }

    // The builders below are `toyos-aml`'s test encodings of §20.2, the few a
    // definition block here needs.

    pub fn sealed(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut table = vec![0u8; 36];
        table[..4].copy_from_slice(signature);
        table[4..8].copy_from_slice(&(36 + body.len() as u32).to_le_bytes());
        table[8] = 2;
        table.extend_from_slice(body);
        table[9] = 0u8.wrapping_sub(table.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)));
        table
    }

    pub fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// `Name (<name>, <value>)` for a four-character name and a byte.
    pub fn name(name: &[u8; 4], value: u8) -> Vec<u8> {
        cat(&[&[0x08], name, &[0x0A, value]])
    }

    /// `Name (_S5_, Package (2) { a, b })`.
    pub fn s5_package(a: u8, b: u8) -> Vec<u8> {
        cat(&[&[0x08], b"_S5_", &[0x12, 0x06, 0x02, 0x0A, a, 0x0A, b]])
    }

    /// A machine whose XSDT lists a FADT naming `dsdt` and then `ssdts`.
    pub fn crafted(dsdt: &[u8], ssdts: &[&[u8]]) -> Scripted {
        const AT: u64 = 0x10_0000;
        let at = |n: usize| AT + 0x1_0000 * n as u64;
        let mut fadt_body = vec![0u8; 116 - 36];
        fadt_body[40 - 36..44 - 36].copy_from_slice(&(at(2) as u32).to_le_bytes());
        let mut fadt = sealed(b"FACP", &fadt_body);
        fadt[8] = 1;
        fadt[9] = 0;
        fadt[9] = 0u8.wrapping_sub(fadt.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)));
        let mut entries = at(1).to_le_bytes().to_vec();
        for n in 0..ssdts.len() {
            entries.extend_from_slice(&at(3 + n).to_le_bytes());
        }
        let xsdt = sealed(b"XSDT", &entries);
        let mut rsdp = vec![0u8; 36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&at(0).to_le_bytes());
        rsdp[8] = 0u8.wrapping_sub(rsdp[..20].iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)));
        rsdp[32] = 0u8.wrapping_sub(rsdp.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)));
        let mut memory = vec![(AT - 0x1000, 9, rsdp), (at(0), 9, xsdt), (at(1), 9, fadt), (at(2), 9, dsdt.to_vec())];
        memory.extend(ssdts.iter().enumerate().map(|(n, ssdt)| (at(3 + n), 9, ssdt.to_vec())));
        Scripted { memory, ..Default::default() }
    }

    pub const CRAFTED_RSDP: u64 = 0x10_0000 - 0x1000;

    #[test]
    fn a_refused_ssdt_is_one_block_refused_and_the_rest_load() {
        let dsdt = sealed(b"DSDT", &cat(&[&name(b"AAAA", 1), &s5_package(5, 7)]));
        let collides = sealed(b"SSDT", &name(b"AAAA", 2));
        let mut unsummed = sealed(b"SSDT", &name(b"BBBB", 3));
        unsummed[9] = unsummed[9].wrapping_add(1);
        let good = sealed(b"SSDT", &name(b"CCCC", 4));
        let kernel = crafted(&dsdt, &[&collides, &unsummed, &good]);
        assert_eq!(
            load_only(&kernel, CRAFTED_RSDP),
            Loaded {
                blocks: vec![Ok(()), Err("a name defined twice".into()), Err("its bytes do not sum to zero".into()), Ok(())],
                s5: Some((5, 7)),
                handed: true,
            }
        );
    }

    #[test]
    fn a_refused_dsdt_leaves_no_namespace_and_loads_no_ssdt() {
        // A definition block that ends inside a Name.
        let dsdt = sealed(b"DSDT", &[0x08, b'A']);
        let ssdt = sealed(b"SSDT", &name(b"CCCC", 4));
        let kernel = crafted(&dsdt, &[&ssdt, &ssdt]);
        let loaded = load_only(&kernel, CRAFTED_RSDP);
        assert_eq!(loaded.blocks.len(), 1, "an SSDT was loaded onto no DSDT: {loaded:?}");
        assert!(matches!(&loaded.blocks[0], Err(kind) if kind.starts_with("malformed AML: ")), "{loaded:?}");
        assert_eq!(loaded.s5, None);
    }

    #[test]
    fn tables_the_kernel_will_not_read_are_refused_by_its_name_for_it() {
        // No RSDP where the claim says: RAM, to the stand-in.
        assert_eq!(load_only(&machine(QEMU), 0x1000), Loaded::default());

        // The DSDT's own bytes are not the firmware's to read.
        let kernel = machine(&QEMU[..QEMU.len() - 1]);
        let ram = "its bytes could not be read: a SystemMemory read the kernel refused UsableMemory, in memory of type 7";
        assert_eq!(load_only(&kernel, RSDP), Loaded { blocks: vec![Err(ram.into())], s5: None, handed: false });
    }

    /// The shape the first load on the real machine had: its RSDP and XSDT
    /// in memory the kernel reads, and its FADT, DSDT and SSDTs in memory of
    /// a type the kernel passed no read of then, here one it still does not. The DSDT's line says that, by
    /// the kernel's name for the refusal and the type, and not that nothing
    /// names a DSDT.
    #[test]
    fn tables_in_memory_of_a_type_the_kernel_keeps_are_refused_by_that_name_and_type() {
        let dsdt = sealed(b"DSDT", &s5_package(5, 0));
        let ssdt = sealed(b"SSDT", &name(b"CCCC", 4));
        let mut kernel = crafted(&dsdt, &[&ssdt, &ssdt]);
        // What the XSDT lists moves into kept memory: the RSDP and the XSDT stay.
        let listed = kernel.memory.split_off(2);
        kernel.kept = listed.iter().map(|(at, _, bytes)| (*at, at + bytes.len() as u64, 5)).collect();
        let kept = "its bytes could not be read: a SystemMemory read the kernel refused MemoryType, in memory of type 5";
        assert_eq!(load_only(&kernel, CRAFTED_RSDP), Loaded { blocks: vec![Err(kept.into())], s5: None, handed: false });
    }

    #[test]
    fn an_s5_that_is_no_package_of_two_integers_is_refused_by_kind() {
        let absent = sealed(b"DSDT", &name(b"AAAA", 1));
        assert_eq!(load_only(&crafted(&absent, &[]), CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: None, handed: false });
        let integer = sealed(b"DSDT", &name(b"_S5_", 5));
        assert_eq!(load_only(&crafted(&integer, &[]), CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: None, handed: false });
        let short = sealed(b"DSDT", &cat(&[&[0x08], b"_S5_", &[0x12, 0x04, 0x01, 0x0A, 0x05]]));
        assert_eq!(load_only(&crafted(&short, &[]), CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: None, handed: false });
    }

    /// `SLP_TYPa` is the package's first element and the only one handed
    /// over; one the register's three bits do not hold is the kernel's to
    /// refuse, and leaves the machine without a power-off, as every `\_S5`
    /// that was not evaluated does.
    #[test]
    fn only_an_s5_the_kernel_kept_is_a_power_off() {
        let kernel = crafted(&sealed(b"DSDT", &s5_package(7, 9)), &[]);
        assert_eq!(load_only(&kernel, CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: Some((7, 9)), handed: true });
        assert_eq!(kernel.handed.get(), Some(7));

        let kernel = crafted(&sealed(b"DSDT", &s5_package(8, 0)), &[]);
        assert_eq!(load_only(&kernel, CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: Some((8, 0)), handed: false });
        assert_eq!(kernel.handed.get(), None);

        for dsdt in [sealed(b"DSDT", &name(b"AAAA", 1)), sealed(b"DSDT", &[0x08, b'A'])] {
            let kernel = crafted(&dsdt, &[]);
            assert!(!load_only(&kernel, CRAFTED_RSDP).handed);
            assert_eq!(kernel.handed.get(), None);
        }
        let kernel = machine(QEMU);
        assert!(!load_only(&kernel, 0x1000).handed);
        assert_eq!(kernel.handed.get(), None);
    }

    #[test]
    fn a_machine_that_stops_mid_load_ends_it_with_nothing_refused() {
        for answered in [0, 3, 40] {
            let kernel = machine(QEMU);
            kernel.stops_after.set(Some(answered));
            assert_eq!(load_only(&kernel, RSDP), Loaded::default(), "after {answered} reads");
        }
    }

    #[test]
    fn every_refusals_kind_is_free_of_what_the_firmware_chose() {
        let chosen = "\\_SB_.PCI0.XYZW";
        for why in [
            Error::NotFound(chosen.into()),
            Error::Exists(chosen.into()),
            Error::Malformed { at: 0x1234, why: "a NameString" },
            Error::Fatal { kind: 1, code: 0x1234, arg: 0x5678 },
            Error::Bridge { segment: 0, bus: 0x12, device: 0x1c, function: 5, header_type: 0x81, secondary: Some(0x34) },
        ] {
            let said = kind(&why);
            assert!(!said.contains("XYZW") && !said.chars().any(|c| c.is_ascii_digit()), "{why:?} is said as {said:?}");
        }
        assert_eq!(kind(&Error::Bridge { segment: 0, bus: 0, device: 0, function: 0, header_type: 0, secondary: None }), BRIDGE);
        assert_eq!(kind(&Error::Host("a write to SystemIO".into())), "denied by this server: a write to SystemIO");
        let unlisted = Refusal::Kernel { space: toyos_abi::acpi::Space::SystemMemory, refused: toyos_abi::acpi::Refused::MemoryType, memory_type: toyos_abi::acpi::UNLISTED };
        assert_eq!(
            unread(&TableError::Unmapped { at: 0xdead_0000, len: 36 }, |at| (at == 0xdead_0000).then_some(unlisted)),
            "its bytes could not be read: a SystemMemory read the kernel refused MemoryType, in unlisted firmware memory"
        );
    }

    /// A Lock field's access takes the Global Lock through the kernel and
    /// gives it back; one the firmware holds is the load's refusal, by name,
    /// and the field is not read without it.
    #[test]
    fn a_lock_field_read_at_load_takes_the_lock_and_a_held_lock_refuses_the_table() {
        const NVS: u64 = 0x7700_0000;
        // OperationRegion (REGN, SystemMemory, 0x77000000, 4); Field (REGN,
        // ByteAcc, Lock, Preserve) { FLDA, 8 }; Name (COPY, 0); Store (FLDA, COPY).
        let body = cat(&[
            &[0x5B, 0x80],
            b"REGN",
            &[0x00, 0x0C, 0x00, 0x00, 0x00, 0x77, 0x0A, 0x04],
            &[0x5B, 0x81, 0x0B],
            b"REGN",
            &[0x11],
            b"FLDA",
            &[0x08],
            &[0x08],
            b"COPY",
            &[0x00],
            &[0x70],
            b"FLDA",
            b"COPY",
            &s5_package(5, 0),
        ]);
        let dsdt = sealed(b"DSDT", &body);
        let with_nvs = || {
            let mut kernel = crafted(&dsdt, &[]);
            kernel.memory.push((NVS, 10, vec![0x42; 4]));
            kernel
        };
        let field = toyos_abi::acpi::Access::read(toyos_abi::acpi::Space::SystemMemory, NVS, toyos_abi::acpi::Width::Byte);

        let kernel = with_nvs();
        assert_eq!(load_only(&kernel, CRAFTED_RSDP), Loaded { blocks: vec![Ok(())], s5: Some((5, 0)), handed: true });
        assert!(!kernel.held.get(), "the load ended holding the Global Lock");
        assert_eq!(kernel.asked.borrow().last(), Some(&field));

        let kernel = with_nvs();
        kernel.takes.borrow_mut().push_back(Take::Pending);
        let loaded = load_only(&kernel, CRAFTED_RSDP);
        assert_eq!(loaded, Loaded { blocks: vec![Err(format!("denied by this server: {HELD}"))], s5: None, handed: false });
        assert_ne!(kernel.asked.borrow().last(), Some(&field), "the field was read without the lock");
    }
}
