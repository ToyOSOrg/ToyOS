//! The namespace read from the root down (§5.3): every object once, after
//! the object it is in, siblings in the order their tables declared them,
//! whatever a table's names sort as, however deep it nests, and whatever a
//! refused load or a method's exit took out of the arena before.

mod common;

use common::*;
use toyos_aml::{Error, Interpreter, Kind, Value};

/// What §5.3.1 and §5.7 predefine, in the order `Interpreter::new` makes them.
const PREDEFINED: [(&str, Kind); 9] = [
    ("\\_GPE", Kind::Scope),
    ("\\_PR_", Kind::Scope),
    ("\\_SB_", Kind::Scope),
    ("\\_SI_", Kind::Scope),
    ("\\_TZ_", Kind::Scope),
    ("\\_GL_", Kind::Mutex),
    ("\\_OSI", Kind::Method),
    ("\\_OS_", Kind::String),
    ("\\_REV", Kind::Integer),
];

/// Every entry's path and kind, after the predefined ones, each checked
/// against its depth and its name.
fn walked(i: &Interpreter) -> Vec<(String, Kind)> {
    let mut w = i.walk().unwrap();
    let mut out = Vec::new();
    while let Some(e) = w.next() {
        let path = w.path();
        assert_eq!(path.len(), 5 * e.depth as usize, "{path}");
        assert_eq!(&path.as_bytes()[path.len() - 4..], &e.name, "{path}");
        out.push((path.to_string(), e.kind));
    }
    let predefined: Vec<(String, Kind)> = PREDEFINED.iter().map(|&(p, k)| (p.to_string(), k)).collect();
    assert_eq!(out[..PREDEFINED.len()], predefined);
    out.split_off(PREDEFINED.len())
}

fn listed(entries: &[(&str, Kind)]) -> Vec<(String, Kind)> {
    entries.iter().map(|&(p, k)| (p.to_string(), k)).collect()
}

fn alias(source: &str, name_: &str) -> Vec<u8> {
    cat(&[&[0x06], &name(source), &name(name_)])
}

/// Names that sort against the order they are declared in, at the root and
/// below it; a second table that adds to a device of the first; a table
/// refused after it made objects, and a method that made some and exited,
/// both before the table whose objects take their slots.
#[test]
fn siblings_are_read_in_the_order_they_were_declared() {
    let (mut i, mut m) = loaded(&cat(&[
        &device("ZZZZ", &cat(&[&def_name("YYYY", &int(1)), &def_name("BBBB", &int(2))])),
        &device("MMMM", &[]),
        &method("MAKE", 0, &cat(&[&device("\\ZZZZ.TMP0", &[]), &def_name("\\AAA0", &int(0))])),
    ]));
    let before = walked(&i);
    assert_eq!(
        before,
        listed(&[
            ("\\ZZZZ", Kind::Device),
            ("\\ZZZZ.YYYY", Kind::Integer),
            ("\\ZZZZ.BBBB", Kind::Integer),
            ("\\MMMM", Kind::Device),
            ("\\MAKE", Kind::Method),
        ])
    );

    // A table that defines three objects and then collides: all three go.
    let refused = table(
        b"SSDT",
        2,
        &cat(&[&device("\\ZZZZ.GONE", &def_name("X", &int(1))), &def_name("\\AAAA", &int(1)), &def_name("\\MMMM", &int(1))]),
    );
    assert_eq!(i.load_bytes(&mut m, &refused), Err(Error::Exists("\\MMMM".into())));
    assert_eq!(walked(&i), before);
    assert_eq!(i.evaluate(&mut m, "\\MAKE", &[]), Ok(Value::Uninitialized));
    assert_eq!(walked(&i), before);

    let ssdt = table(
        b"SSDT",
        2,
        &cat(&[
            &def_name("\\ZZZZ.CCCC", &int(3)),
            &def_name("\\AAAA", &int(4)),
            &scope("\\ZZZZ", &def_name("AAAA", &int(5))),
            &def_name("\\MMMM.KKKK", &int(6)),
        ]),
    );
    i.load_bytes(&mut m, &ssdt).unwrap();
    let after = walked(&i);
    assert_eq!(
        after,
        listed(&[
            ("\\ZZZZ", Kind::Device),
            ("\\ZZZZ.YYYY", Kind::Integer),
            ("\\ZZZZ.BBBB", Kind::Integer),
            ("\\ZZZZ.CCCC", Kind::Integer),
            ("\\ZZZZ.AAAA", Kind::Integer),
            ("\\MMMM", Kind::Device),
            ("\\MMMM.KKKK", Kind::Integer),
            ("\\MAKE", Kind::Method),
            ("\\AAAA", Kind::Integer),
        ])
    );
    // A walked path is one `evaluate` takes.
    let values: Vec<Value> =
        after.iter().filter(|(_, k)| *k == Kind::Integer).map(|(p, _)| i.evaluate(&mut m, p, &[]).unwrap()).collect();
    assert_eq!(values, [1, 2, 3, 5, 6, 4].map(Value::Integer));
}

/// An Alias is no second object: not of a device, whose `_INI` would run
/// twice, and not of an ancestor, below which a walk that followed it would
/// find itself again.
#[test]
fn an_alias_is_not_walked() {
    let (mut i, mut m) = loaded(&cat(&[
        &device(
            "DEV0",
            &cat(&[&def_name("_INI", &int(7)), &device("SUB0", &alias("\\DEV0", "LOOP")), &alias("SUB0", "TWIN")]),
        ),
        &alias("\\DEV0", "ALI0"),
    ]));
    assert_eq!(
        walked(&i),
        listed(&[("\\DEV0", Kind::Device), ("\\DEV0._INI", Kind::Integer), ("\\DEV0.SUB0", Kind::Device)])
    );
    // The aliases are there, and name what the walk met once.
    assert_eq!(i.evaluate(&mut m, "\\ALI0._INI", &[]), Ok(Value::Integer(7)));
    assert_eq!(i.evaluate(&mut m, "\\DEV0.SUB0.LOOP.TWIN.LOOP._INI", &[]), Ok(Value::Integer(7)));
}

/// §5.3.1's scopes are descended and are no device; every other type is
/// what Table 19.36 calls it.
#[test]
fn every_object_is_walked_as_what_it_is() {
    let processor = cat(&[&[0x5B, 0x83], &pkg(&cat(&[&name("CPU0"), &[0x01, 0x10, 0x04, 0x00, 0x00, 0x06]]))]);
    let (i, _) = loaded(&cat(&[
        &scope("\\_SB", &device("PCI0", &method("_INI", 0, &[]))),
        &scope("\\_PR", &processor),
        &thermal_zone("\\_TZ.TZ00", &[]),
        &power_resource("PWR0", &[]),
        &def_name("STR0", &string("s")),
        &def_name("BUF0", &buffer(&int(4), &[])),
        &def_name("PKG0", &package(&[int(1)])),
        &op_region("REG0", 0x00, &int(0), &int(8)),
        &field("REG0", 0x01, &[unit("FLD0", 8)]),
        &cat(&[&[0x8C], &name("BUF0"), &int(0), &name("BFL0")]),
        &cat(&[&[0x5B, 0x01], &name("MTX0"), &[0x00]]),
        &cat(&[&[0x5B, 0x02], &name("EVT0")]),
        &def_name("REF0", &int(0)),
        &method("MKRF", 0, &copy_object(&ref_of(&name("STR0")), &name("REF0"))),
    ]));
    let mut i = i;
    i.evaluate(&mut Machine::default(), "\\MKRF", &[]).unwrap();
    let mut w = i.walk().unwrap();
    let mut seen = Vec::new();
    while let Some(e) = w.next() {
        seen.push((w.path().to_string(), e.depth, e.kind));
    }
    let at = |path: &str| {
        let (_, depth, kind) = seen.iter().find(|(p, ..)| p == path).unwrap_or_else(|| panic!("{path} is not walked"));
        (*depth, *kind)
    };
    assert_eq!(at("\\_SB_"), (1, Kind::Scope));
    assert_eq!(at("\\_SB_.PCI0"), (2, Kind::Device));
    assert_eq!(at("\\_SB_.PCI0._INI"), (3, Kind::Method));
    assert_eq!(at("\\_PR_.CPU0"), (2, Kind::Processor));
    assert_eq!(at("\\_TZ_.TZ00"), (2, Kind::ThermalZone));
    assert_eq!(at("\\PWR0"), (1, Kind::PowerResource));
    assert_eq!(at("\\STR0"), (1, Kind::String));
    assert_eq!(at("\\BUF0"), (1, Kind::Buffer));
    assert_eq!(at("\\PKG0"), (1, Kind::Package));
    assert_eq!(at("\\REG0"), (1, Kind::OperationRegion));
    assert_eq!(at("\\FLD0"), (1, Kind::FieldUnit));
    assert_eq!(at("\\BFL0"), (1, Kind::BufferField));
    assert_eq!(at("\\MTX0"), (1, Kind::Mutex));
    assert_eq!(at("\\EVT0"), (1, Kind::Event));
    assert_eq!(at("\\REF0"), (1, Kind::Reference));
    // What a scope holds follows it, before the scope declared after it.
    let order: Vec<&str> = seen.iter().map(|(p, ..)| p.as_str()).filter(|p| p.starts_with("\\_")).collect();
    assert_eq!(
        order,
        ["\\_GPE", "\\_PR_", "\\_PR_.CPU0", "\\_SB_", "\\_SB_.PCI0", "\\_SB_.PCI0._INI", "\\_SI_", "\\_TZ_", "\\_TZ_.TZ00", "\\_GL_", "\\_OSI", "\\_OS_", "\\_REV"]
    );
}

/// A device in a device, `levels` deep, below the `above` that are there:
/// the devices are reached through Scopes of at most 200 names each, so a
/// table nests no deeper in terms than the interpreter goes.
fn deeper(above: usize, levels: usize) -> Vec<u8> {
    let mut body = Vec::new();
    for _ in 0..levels {
        body = device("D", &body);
    }
    let mut left = above;
    while left > 0 {
        let chunk = if left % 200 == 0 { 200 } else { left % 200 };
        left -= chunk;
        let path = vec!["D"; chunk].join(".");
        body = scope(&if left == 0 { format!("\\{path}") } else { path }, &body);
    }
    body
}

/// A table nests its namespace as deep as it likes, far past what a walk
/// that recursed, or one that kept a path an entry, could follow.
#[test]
fn a_namespace_nested_deep_is_walked_whole() {
    const TABLES: usize = 120;
    const LEVELS: usize = 100;
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    i.load_bytes(&mut m, &dsdt(&deeper(0, LEVELS))).unwrap();
    for t in 1..TABLES {
        i.load_bytes(&mut m, &table(b"SSDT", 2, &deeper(t * LEVELS, LEVELS))).unwrap();
    }
    let mut w = i.walk().unwrap();
    let mut depths = Vec::new();
    while let Some(e) = w.next() {
        if e.name == *b"D___" {
            assert_eq!(e.kind, Kind::Device);
            assert_eq!(w.path().len(), 5 * e.depth as usize);
            depths.push(e.depth as usize);
        }
    }
    assert_eq!(depths, (1..=TABLES * LEVELS).collect::<Vec<_>>());
}

/// A parent of thousands, declared against the order their names sort in.
#[test]
fn a_parent_of_thousands_is_walked_in_their_order() {
    let names: Vec<String> = (0..4000u32).rev().map(|n| format!("{:04X}", 0xA000 + n)).collect();
    let units: Vec<Vec<u8>> = names.iter().map(|n| unit(n, 8)).collect();
    let (i, _) = loaded(&cat(&[&op_region("MEM", 0x00, &int(0), &int(0x1000)), &device("WIDE", &field("\\MEM", 0x01, &units))]));
    let got: Vec<String> = walked(&i).into_iter().filter(|(_, k)| *k == Kind::FieldUnit).map(|(p, _)| p).collect();
    let want: Vec<String> = names.iter().map(|n| format!("\\WIDE.{n}")).collect();
    assert_eq!(got, want);
}
