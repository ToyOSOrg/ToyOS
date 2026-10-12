//! Load (§19.6.76): a definition block loaded at run time from a SystemMemory
//! region, a field unit of one or a Buffer, its objects global and the
//! loading method's own gone at its exit (§5.5.2, §5.5.2.3); a refused block
//! false and nothing of it kept, and a bound the evaluation's refusal.

mod common;

use common::*;
use toyos_aml::{Address, Error, Interpreter, Kind, Value};

/// Where the region the tests load from begins, and how long it is.
const AT: u64 = 0x1_0000;
const LEN: u64 = 0x200;

/// `OperationRegion (BLK0, SystemMemory, AT, LEN)`.
fn region() -> Vec<u8> {
    op_region("BLK0", 0x00, &int(AT), &int(LEN))
}

/// `Load (<source>, <target>)` (§20.2.5.4).
fn load(source: &str, target: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x20], &name(source), target])
}

/// The block every test loads: `\LDED` 0x42, and `\LMTH`, which returns
/// 0x43.
fn block() -> Vec<u8> {
    ssdt_named("LOADED", &cat(&[&def_name("\\LDED", &int(0x42)), &method("\\LMTH", 0, &ret(&int(0x43)))]))
}

/// The namespace of `dsdt` loaded, with `block` in memory at [`AT`].
fn machine(dsdt: &[u8], block: &[u8]) -> (Interpreter, Machine) {
    let mut m = Machine::default();
    m.poke(Address::Memory(AT), block);
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &common::dsdt(dsdt)).expect("the DSDT loads");
    (i, m)
}

fn kept(i: &mut Interpreter, m: &mut Machine) -> (Result<Value, Error>, Result<Value, Error>) {
    (i.evaluate(m, "\\LDED", &[]), i.evaluate(m, "\\LMTH", &[]))
}

fn loaded() -> (Result<Value, Error>, Result<Value, Error>) {
    (Ok(Value::Integer(0x42)), Ok(Value::Integer(0x43)))
}

/// A method that defines objects of its own, loads, and defines more: the
/// load is true, stored to its target and returned; the block's objects
/// outlive the method, its methods run from its bytes after, and the
/// method's own objects are gone at its exit.
#[test]
fn a_block_a_method_loads_outlives_it_and_the_methods_own_objects_do_not() {
    let main = method(
        "MAIN",
        0,
        &cat(&[&def_name("MINE", &int(5)), &store(&load("BLK0", &local(0)), &name("\\RES")), &def_name("MORE", &int(6)), &ret(&local(0))]),
    );
    let (mut i, mut m) = machine(&cat(&[&region(), &def_name("RES", &int(0)), &main]), &block());
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(u64::MAX)));
    assert_eq!(i.evaluate(&mut m, "\\RES", &[]), Ok(Value::Integer(u64::MAX)));
    assert_eq!(kept(&mut i, &mut m), loaded());
    for gone in ["\\MAIN.MINE", "\\MAIN.MORE"] {
        assert_eq!(i.evaluate(&mut m, gone, &[]), Err(Error::NotFound(gone.into())));
    }
    let walked: Vec<String> = {
        let mut walk = i.walk().expect("the namespace walks");
        let mut paths = Vec::new();
        while walk.next().is_some() {
            paths.push(walk.path().to_string());
        }
        paths
    };
    assert!(walked.contains(&"\\LDED".to_string()) && walked.contains(&"\\LMTH".to_string()), "{walked:?}");
    assert_eq!(m.loads(), [Event::Loaded("\\MAIN".into(), Some(*b"LOADED  "), None)]);
}

/// §19.6.76: a block loads at definition-block level as it does from a
/// method, from the region, from a field unit of it at whole bytes, and from
/// a Buffer, each at the root.
#[test]
fn a_block_loads_from_a_region_a_field_unit_and_a_buffer_at_definition_block_level() {
    let bytes = block();
    let sources = [
        cat(&[&region(), &def_name("RES", &int(0)), &load("BLK0", &name("RES"))]),
        cat(&[
            &region(),
            &def_name("RES", &int(0)),
            &field("BLK0", 0x01, &[unit("TABL", 8 * bytes.len())]),
            &load("TABL", &name("RES")),
        ]),
        cat(&[&def_name("BUFF", &buffer(&int(bytes.len() as u64), &bytes)), &def_name("RES", &int(0)), &load("BUFF", &name("RES"))]),
    ];
    for dsdt in sources {
        let (mut i, mut m) = machine(&dsdt, &bytes);
        assert_eq!(i.evaluate(&mut m, "\\RES", &[]), Ok(Value::Integer(u64::MAX)));
        assert_eq!(kept(&mut i, &mut m), loaded());
        assert_eq!(m.loads(), [Event::Loaded("\\".into(), Some(*b"LOADED  "), None)]);
    }
    // A field unit's block, read from where the field begins in its region.
    let shifted = cat(&[&[0u8; 16], &bytes]);
    let dsdt = cat(&[&region(), &def_name("RES", &int(0)), &field("BLK0", 0x01, &[skip(8 * 16), unit("TABL", 8 * bytes.len())]), &load("TABL", &name("RES"))]);
    let (mut i, mut m) = machine(&dsdt, &shifted);
    assert_eq!(i.evaluate(&mut m, "\\RES", &[]), Ok(Value::Integer(u64::MAX)));
    assert!(m.log.contains(&Event::Table(AT + 16, bytes.len())), "{:?}", m.log);
}

/// The value a `\MAIN` that loads `block` from the region into `Local0`
/// returns, and what the namespace kept of the block.
fn refused(block: &[u8], prepare: impl FnOnce(&mut Machine)) -> (Result<Value, Error>, Machine, Interpreter) {
    let main = method("MAIN", 0, &cat(&[&load("BLK0", &local(0)), &ret(&local(0))]));
    let (mut i, mut m) = machine(&cat(&[&region(), &main]), block);
    prepare(&mut m);
    let r = i.evaluate(&mut m, "\\MAIN", &[]);
    (r, m, i)
}

/// §19.6.76: "A value of zero (false) means the operation failed": a block
/// whose bytes do not sum to zero, whose length runs past its region or
/// does not hold its header, whose AML is malformed or collides with a
/// name, or whose bytes the host refuses, loads false; nothing of it is
/// kept, and the host is told why.
#[test]
fn a_refused_block_is_false_and_keeps_nothing() {
    let good = block();
    let mut unsummed = good.clone();
    unsummed[40] ^= 1;
    let mut long = good.clone();
    long[4..8].copy_from_slice(&((LEN + 1) as u32).to_le_bytes());
    let mut short = good.clone();
    short[4..8].copy_from_slice(&20u32.to_le_bytes());
    // Ends inside a Name; and defines `\LDED` twice.
    let malformed = ssdt_named("LOADED", &cat(&[&def_name("\\LDED", &int(0x42)), &[0x08, b'A']]));
    let collides = ssdt_named("LOADED", &cat(&[&def_name("\\LDED", &int(0x42)), &def_name("\\LDED", &int(0x43))]));
    let table = |why: &'static str| Some(Error::Table(why));
    for (bytes, why) in [
        (unsummed, table("a definition block whose bytes do not sum to zero (§5.2.6)")),
        (long, table("a definition block longer than the region it is loaded from (§19.6.76)")),
        (short, table("a definition block whose length does not hold its header, or is not what was read (§5.2.6)")),
        (collides, Some(Error::Exists("\\LDED".into()))),
    ] {
        let (r, m, mut i) = refused(&bytes, |_| ());
        assert_eq!(r, Ok(Value::Integer(0)));
        assert_eq!(m.loads(), [Event::Loaded("\\MAIN".into(), Some(*b"LOADED  "), why)]);
        let mut m = m;
        assert_eq!(i.evaluate(&mut m, "\\LDED", &[]), Err(Error::NotFound("\\LDED".into())));
    }
    let (r, m, _) = refused(&malformed, |_| ());
    assert_eq!(r, Ok(Value::Integer(0)));
    assert!(matches!(&m.loads()[..], [Event::Loaded(_, _, Some(Error::Malformed { .. }))]), "{:?}", m.loads());
    // The host's refusal of a page Table 15.6 calls neither reserved nor NVS.
    let (r, m, _) = refused(&good, |m| m.reclaim = vec![(AT, AT + 0x1000)]);
    assert_eq!(r, Ok(Value::Integer(0)));
    let denied = Error::Host("a definition block in memory Table 15.6 calls neither reserved nor NVS".into());
    assert_eq!(m.loads(), [Event::Loaded("\\MAIN".into(), None, Some(denied))]);
}

/// A block that defines an object below one the loading method made, or an
/// Alias of one, would outlive it: it is refused, false, and keeps nothing;
/// below an object a table made it loads.
#[test]
fn a_block_that_would_outlive_its_parent_or_source_is_refused() {
    let main = method(
        "MAIN",
        0,
        &cat(&[&device("DEVX", &[]), &def_name("MINE", &int(5)), &load("BLK0", &local(0)), &ret(&local(0))]),
    );
    for (body, kept) in [
        (scope("\\MAIN.DEVX", &def_name("LDED", &int(1))), false),
        (cat(&[&[0x06], &name("\\MAIN.MINE"), &name("\\LDED")]), false),
        (scope("\\DEVS", &def_name("LDED", &int(1))), true),
    ] {
        let (mut i, mut m) = machine(&cat(&[&region(), &device("DEVS", &[]), &main]), &ssdt_named("LOADED", &body));
        assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(if kept { u64::MAX } else { 0 })));
        if !kept {
            assert!(matches!(&m.loads()[..], [Event::Loaded(_, _, Some(Error::Rule(_)))]), "{:?}", m.loads());
        }
    }
}

/// A bound the block's load reaches is the evaluation's: steps a load takes
/// are the evaluation's own, and buy it no more.
#[test]
fn a_bound_a_block_reaches_refuses_the_evaluation() {
    let spins = ssdt_named("LOADED", &while_(&int(1), &[]));
    let (r, m, _) = refused(&spins, |_| ());
    assert_eq!(r, Err(Error::Bound("more steps than one evaluation may take")));
    assert!(matches!(&m.loads()[..], [Event::Loaded(_, _, Some(Error::Bound(_)))]), "{:?}", m.loads());
}

/// A loaded block's integers are as wide as the DSDT's revision makes every
/// integer (§19.6.29), whatever its own header's revision.
#[test]
fn a_loaded_blocks_integers_are_the_dsdts_width() {
    let wide = ssdt_named("LOADED", &def_name("\\WIDE", &int(u64::MAX)));
    let mut m = Machine::default();
    m.poke(Address::Memory(AT), &wide);
    let mut i = Interpreter::new();
    let narrow = table(b"DSDT", 1, &cat(&[&region(), &def_name("RES", &int(0)), &load("BLK0", &name("RES"))]));
    i.load_bytes(&mut m, &narrow).expect("the DSDT loads");
    assert_eq!(i.evaluate(&mut m, "\\RES", &[]), Ok(Value::Integer(0xFFFF_FFFF)));
    assert_eq!(i.evaluate(&mut m, "\\WIDE", &[]), Ok(Value::Integer(0xFFFF_FFFF)));
}

/// What the walk reads after a run-time load: the block's objects at their
/// place in definition order, past every object the namespace held before.
#[test]
fn a_loaded_blocks_objects_are_walked_past_what_was_defined_before() {
    let main = method("MAIN", 0, &load("BLK0", &[0x00]));
    let (mut i, mut m) = machine(&cat(&[&region(), &main]), &block());
    let before = i.defined();
    i.evaluate(&mut m, "\\MAIN", &[]).expect("MAIN runs");
    let mut walk = i.walk().expect("the namespace walks");
    let mut serials = Vec::new();
    while let Some(entry) = walk.next() {
        if ["\\LDED", "\\LMTH"].contains(&walk.path()) {
            serials.push((entry.serial >= before, entry.kind));
        }
    }
    assert_eq!(serials, [(true, Kind::Integer), (true, Kind::Method)]);
}

/// A table the XSDT lists, at [`AT`]: signature `OEM1`, OEM ID `TOYOS `,
/// OEM Table ID `TABLE1`, defining `\_SB.TBLD` and `MYD` below its root.
fn listed(m: &mut Machine) -> Vec<u8> {
    let mut t = ssdt_named("TABLE1", &cat(&[&def_name("TBLD", &int(0x51)), &def_name("MYD", &int(0))]));
    t[..4].copy_from_slice(b"OEM1");
    t[9] = 0;
    t[9] = 0u8.wrapping_sub(t.iter().fold(0u8, |s, &b| s.wrapping_add(b)));
    m.poke(Address::Memory(AT), &t);
    m.listed = vec![AT];
    t
}

/// `LoadTable (<sig>, <oem id>, <table id>, <root>, <parameter path>, <parameter data>)`.
fn load_table(ids: [&str; 3], root: &str, parameter: &str, data: &[u8]) -> Vec<u8> {
    cat(&[&[0x5B, 0x1F], &string(ids[0]), &string(ids[1]), &string(ids[2]), &string(root), &string(parameter), data])
}

fn table_machine(body: &[u8]) -> (Interpreter, Machine) {
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &common::dsdt(body)).expect("the DSDT loads");
    listed(&mut m);
    (i, m)
}

/// §19.6.77: the table found by its three identifiers, a shorter one padded,
/// loads below the RootPathString; ParameterData is stored to the
/// ParameterPathString below that root once it has, and the value is true.
#[test]
fn load_table_loads_the_listed_table_below_its_root_and_stores_its_parameter() {
    let main = method("MAIN", 0, &ret(&load_table(["OEM1", "TOYOS", "TABLE1"], "\\_SB", "MYD", &int(0x77))));
    let (mut i, mut m) = table_machine(&main);
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(u64::MAX)));
    assert_eq!(i.evaluate(&mut m, "\\_SB.TBLD", &[]), Ok(Value::Integer(0x51)));
    assert_eq!(i.evaluate(&mut m, "\\_SB.MYD", &[]), Ok(Value::Integer(0x77)));
    assert!(m.log.contains(&Event::FindTable(b"OEM1".to_vec(), b"TOYOS".to_vec(), b"TABLE1".to_vec())));
    assert_eq!(m.loads(), [Event::Loaded("\\MAIN".into(), Some(*b"TABLE1  "), None)]);
}

/// No table of those identifiers loads false and asks nothing more; one
/// whose memory the host denies, as Table 15.6 has it, loads false and
/// keeps nothing; an identifier longer than its field is a run-time error,
/// and so is a parameter path that names nothing once the table loaded.
#[test]
fn load_table_of_no_table_or_denied_memory_is_false_and_a_long_identifier_is_refused() {
    let call = |ids: [&str; 3], parameter: &str| method("MAIN", 0, &ret(&load_table(ids, "", parameter, &int(1))));
    let (mut i, mut m) = table_machine(&call(["OEM1", "TOYOS", "TABLE2"], ""));
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(0)));
    assert_eq!(m.loads(), []);
    let (mut i, mut m) = table_machine(&call(["OEM1", "TOYOS", "TABLE1"], ""));
    m.reclaim = vec![(AT, AT + 0x1000)];
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Ok(Value::Integer(0)));
    assert_eq!(i.evaluate(&mut m, "\\TBLD", &[]), Err(Error::NotFound("\\TBLD".into())));
    assert!(matches!(&m.loads()[..], [Event::Loaded(_, None, Some(Error::Host(_)))]), "{:?}", m.loads());
    for ids in [["OEM12", "TOYOS", "TABLE1"], ["OEM1", "TOYOSXX", "TABLE1"], ["OEM1", "TOYOS", "TABLE123X"]] {
        let (mut i, mut m) = table_machine(&call(ids, ""));
        assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Err(Error::Rule("a table's signature, OEM ID or OEM Table ID longer than its field (§19.6.77)")), "{ids:?}");
    }
    let (mut i, mut m) = table_machine(&call(["OEM1", "TOYOS", "TABLE1"], "NONE"));
    assert_eq!(i.evaluate(&mut m, "\\MAIN", &[]), Err(Error::NotFound("\\NONE".into())));
}

/// §19.6.25: a Data Table Region is a SystemMemory region over the table
/// the XSDT lists, its header to its last byte; one naming no table, or a
/// table in memory the host denies, refuses the definition.
#[test]
fn a_data_table_region_is_the_listed_tables_memory() {
    // DataTableRegion (DTR0, "OEM1", "TOYOS", "TABLE1"); Field (DTR0,
    // ByteAcc) { SIG, 32, LEN, 32 }.
    let region = |table: &str| {
        cat(&[
            &[0x5B, 0x88],
            &name("DTR0"),
            &string("OEM1"),
            &string("TOYOS"),
            &string(table),
            &field("DTR0", 0x01, &[unit("SIG", 32), unit("LEN", 32), skip(8 * 28), unit("LAST", 8)]),
        ])
    };
    let (mut i, mut m) = {
        let mut m = Machine::default();
        let t = listed(&mut m);
        let mut i = Interpreter::new();
        i.load_bytes(&mut m, &common::dsdt(&region("TABLE1"))).expect("the DSDT loads");
        assert_eq!(i.evaluate(&mut m, "\\LEN", &[]), Ok(Value::Integer(t.len() as u64)));
        (i, m)
    };
    assert_eq!(i.evaluate(&mut m, "\\SIG", &[]), Ok(Value::Integer(u64::from(u32::from_le_bytes(*b"OEM1")))));
    assert!(m.accesses().contains(&Event::Read(Address::Memory(AT), toyos_aml::Access::Byte)));
    let mut m = Machine::default();
    listed(&mut m);
    let refused = Interpreter::new().load_bytes(&mut m, &common::dsdt(&region("TABLE2")));
    assert_eq!(refused, Err(Error::Rule("a DataTableRegion names a table the XSDT does not list (§19.6.25)")));
    let mut m = Machine::default();
    listed(&mut m);
    m.reclaim = vec![(AT, AT + 0x1000)];
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &common::dsdt(&region("TABLE1"))), Err(Error::Host(_))));
}
