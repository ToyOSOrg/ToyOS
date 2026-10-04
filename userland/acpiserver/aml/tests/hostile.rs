//! A table is firmware's, and untrusted: malformed or hostile bytes are
//! refused by name, and every evaluation ends.

mod common;

use common::*;
use toyos_aml::{Error, Interpreter, Value};

const ZERO: &[u8] = &[0x00];

fn load(body: &[u8]) -> Result<(), Error> {
    Interpreter::new().load(&mut Machine::default(), &dsdt(body))
}

#[test]
fn a_package_length_that_leaves_its_package_is_refused() {
    // A Scope whose PkgLength claims ten bytes more than the table holds.
    let mut s = scope("\\", &def_name("A", &int(1)));
    s[1] += 10;
    assert!(matches!(load(&s), Err(Error::Malformed { .. })));
    // §20.2.4: bits 5-4 of a multi-byte lead byte must be zero.
    let bad = cat(&[&[0x10, 0x70, 0x00], &name("\\")]);
    assert!(matches!(load(&bad), Err(Error::Malformed { .. })));
}

#[test]
fn names_and_strings_hold_only_what_their_encoding_allows() {
    assert!(matches!(load(&cat(&[&[0x08], b"a___", &int(1)])), Err(Error::Malformed { .. })));
    assert!(matches!(load(&cat(&[&[0x08], b"1___", &int(1)])), Err(Error::Malformed { .. })));
    assert!(matches!(load(&cat(&[&[0x08, 0x2F, 0x00], &int(1)])), Err(Error::Malformed { .. })));
    assert!(matches!(load(&def_name("S", &[0x0D, b'a', 0x80, 0x00])), Err(Error::Malformed { .. })));
    assert!(matches!(load(&def_name("S", &[0x0D, b'a'])), Err(Error::Malformed { .. })));
    // A DataObject is all Name takes (§20.2.5.1).
    assert!(matches!(load(&def_name("L", &local(0))), Err(Error::Malformed { .. })));
    // A term list holds terms; data alone is none (§20.2.5).
    assert!(matches!(load(&int(5)), Err(Error::Malformed { .. })));
    assert!(matches!(load(&[0xA1, 0x01]), Err(Error::Malformed { .. })));
    // A name alone in a term list must name a method.
    assert!(matches!(load(&cat(&[&def_name("A", &int(1)), &name("A")])), Err(Error::Malformed { .. })));
}

#[test]
fn a_loop_without_end_is_bounded_in_steps() {
    assert!(matches!(returns(&while_(&int(1), &[0xA3])), Err(Error::Bound(_))));
    assert!(matches!(load(&while_(&int(1), &[])), Err(Error::Bound(_))));
}

#[test]
fn a_loop_that_sleeps_is_bounded_in_time_asked() {
    let (mut i, mut m) = loaded(&method("NAP", 0, &while_(&int(1), &cat(&[&[0x5B, 0x22], &int(1000)]))));
    assert!(matches!(i.evaluate(&mut m, "\\NAP", &[]), Err(Error::Bound(_))));
    let slept: u64 = m.log.iter().map(|e| if let Event::Sleep(ms) = e { *ms } else { 0 }).sum();
    assert!(slept <= 10_000, "{slept}");
}

#[test]
fn nesting_is_bounded_before_the_stack() {
    let mut e = int(1);
    for _ in 0..20_000 {
        e = add(&e, &int(1), ZERO);
    }
    assert!(matches!(returns(&ret(&e)), Err(Error::Bound(_))));
    let mut p = int(1);
    for _ in 0..20_000 {
        p = package(&[p]);
    }
    assert!(matches!(load(&def_name("P", &p)), Err(Error::Bound(_))));
    let forever = method("R", 0, &ret(&name("R")));
    let (mut i, mut m) = loaded(&forever);
    assert!(matches!(i.evaluate(&mut m, "\\R", &[]), Err(Error::Bound(_))));
}

#[test]
fn objects_are_bounded_in_size() {
    assert!(matches!(returns(&ret(&buffer(&ones(), &[]))), Err(Error::Bound(_))));
    assert!(matches!(returns(&ret(&var_package(&ones(), &[]))), Err(Error::Bound(_))));
    // Doubling a buffer until it is larger than this interpreter holds.
    let grow = cat(&[
        &store(&buffer(&int(1), &[1]), &local(0)),
        &while_(&int(1), &op2(0x73, &local(0), &local(0), &local(0))),
    ]);
    assert!(matches!(returns(&grow), Err(Error::Bound(_))));
    // A package stored into itself, nesting one deeper each time.
    let nest = cat(&[
        &store(&package(&[int(0)]), &local(0)),
        &while_(&int(1), &store(&local(0), &index(&local(0), &int(0), ZERO))),
    ]);
    assert!(matches!(returns(&nest), Err(Error::Bound(_))));
}

#[test]
fn a_field_beyond_what_is_held_is_refused() {
    let huge = cat(&[&op_region("MEM", 0x00, &int(0), &ones()), &field("MEM", 1, &[unit("HUGE", 0x0FFF_FFFF)])]);
    let (mut i, mut m) = loaded(&huge);
    assert!(matches!(i.evaluate(&mut m, "\\HUGE", &[]), Err(Error::Bound(_))));
}

#[test]
fn a_caller_value_is_refused_where_it_is_malformed() {
    let (mut i, mut m) = loaded(&method("ONE", 1, &ret(&arg(0))));
    assert!(matches!(i.evaluate(&mut m, "\\ONE", &[Value::String(b"a\0b".to_vec())]), Err(Error::Type(_))));
    assert!(matches!(i.evaluate(&mut m, "\\ONE", &[Value::Reference("\\NONE".into())]), Err(Error::NotFound(_))));
    assert!(matches!(i.evaluate(&mut m, "\\ONE.TOOLONG", &[]), Err(Error::Rule(_))));
}

/// A table holding a little of everything, for the mutations below.
fn seed() -> Vec<u8> {
    cat(&[
        &scope(
            "\\_SB",
            &cat(&[
                &device(
                    "PCI0",
                    &cat(&[
                        &def_name("_BBN", &int(0)),
                        &device(
                            "LPCB",
                            &cat(&[
                                &def_name("_ADR", &int(0x001F_0000)),
                                &op_region("LPCR", 0x02, &int(0x40), &int(0x10)),
                                &field("LPCR", 1, &[unit("R40", 8), unit("R41", 8)]),
                            ]),
                        ),
                    ]),
                ),
                &op_region("MEM0", 0x00, &int(0x1000), &int(0x20)),
                &field("MEM0", 0x03, &[unit("A", 4), unit("B", 12), skip(16), unit("C", 32), unit("D", 64)]),
                &field("MEM0", 0x01, &[skip(128), unit("IDX", 8), unit("DAT", 8)]),
                &index_field("IDX", "DAT", 0x01, &[unit("F0", 1), skip(15), unit("F1", 8)]),
                &def_name("BUF", &buffer(&int(8), &[1, 2, 3, 4])),
                &cat(&[&[0x8A], &name("BUF"), &int(2), &name("BF")]),
                &def_name("PKG", &package(&[int(1), string("two"), buffer(&int(1), &[3]), name("BUF"), package(&[int(4)])])),
                &cat(&[&[0x5B, 0x01], &name("MUT"), &[0x01]]),
            ]),
        ),
        &def_name("_S5", &package(&[int(7), int(7), int(0), int(0)])),
        &method(
            "MAIN",
            2,
            &cat(&[
                &store(&int(0), &local(0)),
                &while_(
                    &lless(&local(0), &int(4)),
                    &cat(&[
                        &increment(&local(0)),
                        &if_(&lequal(&local(0), &int(2)), &store(&name("\\_SB.A"), &local(1))),
                        &else_(&store(&op2(0x73, &string("x"), &local(0), ZERO), &local(2))),
                    ]),
                ),
                &store(&add(&arg(0), &arg(1), ZERO), &name("\\_SB.C")),
                &store(&int(1), &name("\\_SB.F1")),
                &store(&deref(&index(&name("\\_SB.PKG"), &int(4), ZERO)), &local(3)),
                &cat(&[&[0x5B, 0x23], &name("\\_SB.MUT"), &[0xFF, 0xFF]]),
                &cat(&[&[0x5B, 0x27], &name("\\_SB.MUT")]),
                &ret(&cat(&[&[0x89], &name("\\_SB.PKG"), &[1], &int(1), &[0], &int(0), &int(0)])),
            ]),
        ),
        &method("OSI", 0, &ret(&cat(&[&name("\\_OSI"), &string("Windows 2022")]))),
    ])
}

/// xorshift64: deterministic, so a failure names its iteration.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const INTERESTING: [u8; 12] = [0x00, 0x01, 0x0A, 0x0E, 0x11, 0x12, 0x14, 0x40, 0x5B, 0x7F, 0xA2, 0xFF];

fn mutate(body: &mut Vec<u8>, r: &mut Rng) {
    for _ in 0..1 + r.below(4) {
        let at = r.below(body.len());
        match r.below(5) {
            0 => body[at] ^= 1 << r.below(8),
            1 => body[at] = INTERESTING[r.below(INTERESTING.len())],
            2 => body[at] = r.next() as u8,
            3 => body.insert(at, INTERESTING[r.below(INTERESTING.len())]),
            _ => {
                body.remove(at);
            }
        }
    }
}

/// Mutated tables, each loaded and every object of the seed evaluated: a
/// value or a refusal, never a panic, and always an end.
#[test]
fn mutated_tables_yield_a_value_or_a_refusal() {
    let seed = seed();
    {
        let (mut i, mut m) = loaded(&seed);
        assert_eq!(i.evaluate(&mut m, "\\MAIN", &[Value::Integer(1), Value::Integer(2)]), Ok(Value::Integer(0)));
    }
    let paths = ["\\MAIN", "\\OSI", "\\_S5", "\\_SB.PKG", "\\_SB.BF", "\\_SB.A", "\\_SB.F1", "\\_SB.PCI0.LPCB.R41"];
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let (mut loads, mut values) = (0, 0);
    for _ in 0..20_000 {
        let mut body = seed.clone();
        mutate(&mut body, &mut r);
        let mut m = Machine::default();
        let mut i = Interpreter::new();
        if i.load(&mut m, &dsdt(&body)).is_err() {
            continue;
        }
        loads += 1;
        for p in paths {
            let args = if p == "\\MAIN" { vec![Value::Integer(1), Value::Integer(2)] } else { vec![] };
            if i.evaluate(&mut m, p, &args).is_ok() {
                values += 1;
            }
        }
    }
    // The mutations reach past the header into the interpreter.
    assert!(loads > 0 && values > 0, "{loads} loads, {values} values");
}

/// Every prefix of the seed, as a table of its own.
#[test]
fn every_truncation_yields_a_value_or_a_refusal() {
    let seed = seed();
    for n in 0..seed.len() {
        let mut m = Machine::default();
        let mut i = Interpreter::new();
        if i.load(&mut m, &dsdt(&seed[..n])).is_ok() {
            let _ = i.evaluate(&mut m, "\\MAIN", &[Value::Integer(1), Value::Integer(2)]);
        }
    }
}
