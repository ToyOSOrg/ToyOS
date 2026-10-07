//! Loading definition blocks into the namespace (§5.2.6, §5.3, §5.4.2,
//! §5.5.2.3, §20.2.5.1-2).

mod common;

use common::*;
use toyos_aml::{Access, Address, Error, Interpreter, Value};

fn int_of(i: &mut Interpreter, m: &mut Machine, path: &str) -> Result<Value, Error> {
    i.evaluate(m, path, &[])
}

/// A table's length and checksum are `toyos_acpi::Table::open`'s; what the
/// interpreter decides is which tables load, and in which order.
#[test]
fn a_dsdt_loads_first_and_then_ssdts() {
    let mut m = Machine::default();
    let good = dsdt(&def_name("A", &int(1)));
    let facp = table(b"FACP", 2, &def_name("A", &int(1)));
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &facp), Err(Error::Table(_))));

    let ssdt = table(b"SSDT", 2, &def_name("A", &int(1)));
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &ssdt), Err(Error::Table(_))));

    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &good).unwrap();
    assert!(matches!(i.load_bytes(&mut m, &good), Err(Error::Table(_))));
    i.load_bytes(&mut m, &table(b"SSDT", 2, &def_name("B", &int(2)))).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\B", &[]), Ok(Value::Integer(2)));
}

#[test]
fn the_predefined_objects_are_there_before_any_table() {
    let (mut i, mut m) = loaded(&cat(&[
        &method("TSB", 0, &ret(&object_type(&name("\\_SB")))),
        &method("TGL", 0, &ret(&object_type(&name("\\_GL")))),
        &method("TOSI", 0, &ret(&object_type(&name("\\_OSI")))),
    ]));
    // The owner's ruling (2026-10-05): "Microsoft Windows NT", as Windows answers.
    assert_eq!(i.evaluate(&mut m, "\\_OS", &[]), Ok(s("Microsoft Windows NT")));
    assert_eq!(i.evaluate(&mut m, "\\_REV", &[]), Ok(Value::Integer(2)));
    // Table 19.36: a predefined scope is typeless, \_GL a Mutex, \_OSI a Method.
    assert_eq!(i.evaluate(&mut m, "\\TSB", &[]), Ok(Value::Integer(0)));
    assert_eq!(i.evaluate(&mut m, "\\TGL", &[]), Ok(Value::Integer(9)));
    assert_eq!(i.evaluate(&mut m, "\\TOSI", &[]), Ok(Value::Integer(8)));
}

#[test]
fn a_dsdt_below_revision_2_makes_every_integer_32_bits() {
    let body = cat(&[&def_name("ONES", &ones()), &method("MAIN", 0, &ret(&add(&int(0xFFFF_FFFF), &int(2), &[0])))]);
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &table(b"DSDT", 1, &body)).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\ONES", &[]), Ok(Value::Integer(0xFFFF_FFFF)));
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(1)));
    // §19.6.29: the DSDT's revision decides for every SSDT too.
    i.load_bytes(&mut m, &table(b"SSDT", 2, &def_name("SONE", &ones()))).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\SONE", &[]), Ok(Value::Integer(0xFFFF_FFFF)));
}

/// §5.3: "Access using a single segment name (_CRS) will actually access the
/// \_SB_.PCI0._CRS object", and the absolute name errors.
#[test]
fn a_lone_name_is_searched_for_toward_the_root_and_a_path_is_not() {
    let (mut i, mut m) = loaded(&scope(
        "\\_SB",
        &device(
            "PCI0",
            &cat(&[
                &def_name("_CRS", &int(7)),
                &device(
                    "IDE0",
                    &cat(&[
                        &method("LONE", 0, &ret(&name("_CRS"))),
                        &method("ABS", 0, &ret(&name("\\_SB.PCI0.IDE0._CRS"))),
                        &method("UP", 0, &ret(&name("^_CRS"))),
                        &method("DUAL", 0, &ret(&name("PCI0._CRS"))),
                    ]),
                ),
            ]),
        ),
    ));
    assert_eq!(int_of(&mut i, &mut m, "\\_SB.PCI0.IDE0.LONE"), Ok(Value::Integer(7)));
    assert_eq!(int_of(&mut i, &mut m, "\\_SB.PCI0.IDE0.ABS"), Err(Error::NotFound("\\_SB_.PCI0.IDE0._CRS".into())));
    // A method's names resolve from the method itself (§19.6.84), so one
    // prefix reaches IDE0, which holds no _CRS.
    assert!(matches!(int_of(&mut i, &mut m, "\\_SB.PCI0.IDE0.UP"), Err(Error::NotFound(_))));
    // "XYZ.ABCD //search rules do not apply" (§5.3).
    assert!(matches!(int_of(&mut i, &mut m, "\\_SB.PCI0.IDE0.DUAL"), Err(Error::NotFound(_))));
}

#[test]
fn parent_prefixes_climb_one_scope_each() {
    let (mut i, mut m) = loaded(&scope(
        "\\_SB",
        &cat(&[
            &def_name("TOP", &int(1)),
            &device(
                "A",
                &cat(&[
                    &def_name("MID", &int(2)),
                    &device("B", &method("GET", 0, &ret(&add(&name("^^MID"), &name("^^^TOP"), &[0])))),
                ]),
            ),
        ]),
    ));
    assert_eq!(int_of(&mut i, &mut m, "\\_SB.A.B.GET"), Ok(Value::Integer(3)));
}

#[test]
fn a_prefix_above_the_root_finds_nothing() {
    let (mut i, mut m) = loaded(&method("MAIN", 0, &ret(&name("^^^^X"))));
    assert!(matches!(int_of(&mut i, &mut m, "\\MAIN"), Err(Error::NotFound(_))));
}

/// §5.3: "Object XYZ must already exist for the ABCD object to be created".
#[test]
fn a_definition_needs_every_segment_but_its_last() {
    let mut m = Machine::default();
    let r = Interpreter::new().load_bytes(&mut m, &dsdt(&def_name("\\XYZ.ABCD", &int(1))));
    assert!(matches!(r, Err(Error::NotFound(_))));
    let (mut i, mut m) = loaded(&cat(&[&device("\\XYZ", &[]), &def_name("\\XYZ.ABCD", &int(1))]));
    assert_eq!(int_of(&mut i, &mut m, "\\XYZ.ABCD"), Ok(Value::Integer(1)));
}

#[test]
fn a_collision_refuses_the_table_and_leaves_none_of_it() {
    let (mut i, mut m) = loaded(&def_name("A", &int(1)));
    let ssdt = table(b"SSDT", 2, &cat(&[&device("\\NEW", &def_name("X", &int(2))), &def_name("\\A", &int(3))]));
    assert_eq!(i.load_bytes(&mut m, &ssdt), Err(Error::Exists("\\A___".into())));
    assert!(matches!(int_of(&mut i, &mut m, "\\NEW.X"), Err(Error::NotFound(_))));
    assert!(matches!(int_of(&mut i, &mut m, "\\NEW"), Err(Error::NotFound(_))));
    assert_eq!(int_of(&mut i, &mut m, "\\A"), Ok(Value::Integer(1)));
}

#[test]
fn a_scope_opens_only_what_has_one() {
    let mut m = Machine::default();
    let r = Interpreter::new().load_bytes(&mut m, &dsdt(&cat(&[&def_name("NUM", &int(1)), &scope("NUM", &[])])));
    assert!(matches!(r, Err(Error::Type(_))));
    let (mut i, mut m) = loaded(&cat(&[
        &thermal_zone("\\_TZ.TZ0", &[]),
        &power_resource("\\PWR", &[]),
        &scope("\\_TZ.TZ0", &def_name("T", &int(1))),
        &scope("\\PWR", &def_name("P", &int(2))),
        &scope("\\", &def_name("R", &int(3))),
    ]));
    assert_eq!(int_of(&mut i, &mut m, "\\_TZ.TZ0.T"), Ok(Value::Integer(1)));
    assert_eq!(int_of(&mut i, &mut m, "\\PWR.P"), Ok(Value::Integer(2)));
    assert_eq!(int_of(&mut i, &mut m, "\\R"), Ok(Value::Integer(3)));
}

#[test]
fn an_alias_acts_exactly_as_its_source() {
    let alias = cat(&[&[0x06], &name("\\SRC"), &name("ALI")]);
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("SRC", &int(4)),
        &alias,
        &method("MAIN", 0, &cat(&[&store(&int(9), &name("ALI")), &ret(&name("SRC"))])),
    ]));
    assert_eq!(int_of(&mut i, &mut m, "\\ALI"), Ok(Value::Integer(4)));
    assert_eq!(int_of(&mut i, &mut m, "\\MAIN"), Ok(Value::Integer(9)));
    let mut m = Machine::default();
    let missing = cat(&[&[0x06], &name("\\NONE"), &name("ALI")]);
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &dsdt(&missing)), Err(Error::NotFound(_))));
}

#[test]
fn an_external_defines_nothing() {
    let external = cat(&[&[0x15], &name("\\_SB.PCI0.XYZ"), &[0x08, 0x02]]);
    let (mut i, mut m) = loaded(&external);
    assert!(matches!(int_of(&mut i, &mut m, "\\_SB.PCI0.XYZ"), Err(Error::NotFound(_))));
    let mut m = Machine::default();
    // An ArgumentCount above 7 tells a disassembler nothing it could use,
    // and defines nothing either.
    let odd = cat(&[&[0x15], &name("XYZ"), &[0x08, 0x08]]);
    assert!(Interpreter::new().load_bytes(&mut m, &dsdt(&odd)).is_ok());
}

/// The owner's ruling (2026-10-05): "Parse Processor and other legacy
/// constructs real tables still contain, per their last spec definition";
/// ACPI 6.3A §20.2.5.2, §19.6.108, Table 19.36.
#[test]
fn a_processor_parses_opens_a_scope_and_takes_notify() {
    let processor = cat(&[
        &[0x5B, 0x83],
        &pkg(&cat(&[&name("CPU0"), &[0x01, 0x10, 0x04, 0x00, 0x00, 0x06], &def_name("_PPC", &int(3))])),
    ]);
    let (mut i, mut m) = loaded(&cat(&[
        &scope("\\_PR", &processor),
        &method("TYPE", 0, &ret(&object_type(&name("\\_PR.CPU0")))),
        &method("NOTE", 0, &cat(&[&[0x86], &name("\\_PR.CPU0"), &int(0x80)])),
    ]));
    assert_eq!(i.evaluate(&mut m, "\\_PR.CPU0._PPC", &[]), Ok(Value::Integer(3)));
    assert_eq!(i.evaluate(&mut m, "\\TYPE", &[]), Ok(Value::Integer(12)));
    i.evaluate(&mut m, "\\NOTE", &[]).unwrap();
    assert_eq!(m.log, vec![Event::Notify("\\_PR_.CPU0".into(), 0x80)]);
}

const QEMU_DSDT: &[u8] = include_bytes!("../../../../toyos-acpi/fixtures/qemu-11.1.1/dsdt.bin");

/// QEMU 11.1.1's DSDT, as `toyos-acpi/fixtures/qemu-11.1.1/SOURCE` records
/// it: a boot of it logged `ACPI: PM1a=0x604 SLP_TYPa=0`, which the kernel
/// read by `toyos_acpi::s5_slp_typ`'s byte scan.
#[test]
fn qemus_dsdt_loads_and_its_s5_is_what_its_boot_logged() {
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, QEMU_DSDT).unwrap();
    let zero = Value::Integer(0);
    assert_eq!(i.evaluate(&mut m, "\\_S5", &[]), Ok(Value::Package(vec![zero.clone(), zero.clone(), zero.clone(), zero])));
    let scanned = toyos_acpi::s5_slp_typ(&toyos_acpi::Table::open(Image(QEMU_DSDT), 0, b"DSDT", 0).unwrap());
    assert_eq!(scanned, toyos_acpi::S5::SlpTyp(0));
    assert_eq!(m.log, vec![]);
}

/// QEMU's own methods, run against the registers they read: a CPU's `_STA`
/// selects it and reads its enabled bit (QEMU's `docs/specs/acpi_cpu_hotplug.rst`),
/// the HPET's reads its vendor and its period (IA-PC HPET 1.0a §2.3.4).
#[test]
fn qemus_methods_run_against_the_registers_they_read() {
    let (select, flags) = (Address::Io(0xCD8), Address::Io(0xCDC));
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, QEMU_DSDT).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\_SB.CPUS.C001._STA", &[]), Ok(Value::Integer(0)));
    assert_eq!(m.accesses(), vec![Event::Write(select, Access::DWord, 1), Event::Read(flags, Access::Byte)]);
    m.poke(flags, &[1]);
    assert_eq!(i.evaluate(&mut m, "\\_SB.CPUS.C001._STA", &[]), Ok(Value::Integer(0xF)));

    let hpet = Address::Memory(0xFED0_0000);
    assert_eq!(i.evaluate(&mut m, "\\_SB.HPET._STA", &[]), Ok(Value::Integer(0)));
    // Vendor 0x8086 in bits 31:16, a period of 10 ns in femtoseconds above them.
    m.poke(hpet, &[0x01, 0xA2, 0x86, 0x80, 0x80, 0x96, 0x98, 0x00]);
    assert_eq!(i.evaluate(&mut m, "\\_SB.HPET._STA", &[]), Ok(Value::Integer(0xF)));
}

#[test]
fn definition_block_level_code_runs_at_load() {
    let (mut i, mut m) = loaded(&cat(&[
        &if_(&lequal(&name("\\_REV"), &int(2)), &def_name("\\YES", &int(1))),
        &else_(&def_name("\\NO", &int(1))),
        &method("CFG", 0, &ret(&int(1))),
        &if_(&lequal(&name("CFG"), &int(1)), &device("\\DEV", &[])),
    ]));
    assert_eq!(int_of(&mut i, &mut m, "\\YES"), Ok(Value::Integer(1)));
    assert!(matches!(int_of(&mut i, &mut m, "\\NO"), Err(Error::NotFound(_))));
    assert_eq!(int_of(&mut i, &mut m, "\\DEV"), Ok(Value::Reference("\\DEV_".into())));
}

/// The example of §5.5.2.3, with CREG and DREG as names of their own.
#[test]
fn names_a_method_creates_go_when_it_exits() {
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("CREG", &int(0)),
        &def_name("DREG", &int(0)),
        &scope(
            "\\",
            &device(
                "XYZ",
                &cat(&[
                    &def_name("BAR", &int(5)),
                    &method(
                        "FOO",
                        1,
                        &cat(&[
                            &store(&name("BAR"), &name("CREG")),
                            &def_name("BAR", &int(7)),
                            &store(&name("BAR"), &name("DREG")),
                            &def_name("\\XYZ.FOOB", &int(3)),
                            &ret(&name("\\XYZ.FOOB")),
                        ]),
                    ),
                ]),
            ),
        ),
    ]));
    for _ in 0..2 {
        assert_eq!(i.evaluate(&mut m, "\\XYZ.FOO", &[Value::Integer(0)]), Ok(Value::Integer(3)));
        assert_eq!(int_of(&mut i, &mut m, "\\CREG"), Ok(Value::Integer(5)));
        assert_eq!(int_of(&mut i, &mut m, "\\DREG"), Ok(Value::Integer(7)));
        assert!(matches!(int_of(&mut i, &mut m, "\\XYZ.FOOB"), Err(Error::NotFound(_))));
        assert!(matches!(int_of(&mut i, &mut m, "\\XYZ.FOO.BAR"), Err(Error::NotFound(_))));
    }
}

/// §19.6.101: a named reference to data is resolved to its value at run
/// time; a name defined after the package is resolved when read.
#[test]
fn a_package_names_its_elements() {
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("INT1", &int(0x1234)),
        &device("DEV0", &[]),
        &def_name("PKG1", &package(&[int(0x3400), name("\\INT1"), name("\\DEV0"), name("\\LATE")])),
        &def_name("LATE", &int(5)),
    ]));
    assert_eq!(
        int_of(&mut i, &mut m, "\\PKG1"),
        Ok(Value::Package(vec![
            Value::Integer(0x3400),
            Value::Integer(0x1234),
            Value::Reference("\\DEV0".into()),
            Value::Integer(5),
        ]))
    );
    let (mut i, mut m) = loaded(&def_name("PKG2", &package(&[name("\\NONE")])));
    assert!(matches!(int_of(&mut i, &mut m, "\\PKG2"), Err(Error::NotFound(_))));
}

#[test]
fn a_caller_names_only_absolute_paths_and_passes_the_declared_arguments() {
    let (mut i, mut m) = loaded(&method("TWO", 2, &ret(&add(&arg(0), &arg(1), &[0]))));
    assert!(matches!(i.evaluate(&mut m, "TWO", &[]), Err(Error::Rule(_))));
    assert!(matches!(i.evaluate(&mut m, "\\TWO", &[Value::Integer(1)]), Err(Error::Rule(_))));
    assert_eq!(i.evaluate(&mut m, "\\TWO", &[Value::Integer(1), Value::Integer(2)]), Ok(Value::Integer(3)));
    assert!(matches!(i.evaluate(&mut m, "\\_OS", &[Value::Integer(1)]), Err(Error::Rule(_))));
    assert!(matches!(Interpreter::new().evaluate(&mut m, "\\_OS", &[]), Err(Error::Table(_))));
}
