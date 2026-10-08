//! A table is firmware's, and untrusted: malformed or hostile bytes are
//! refused by name, and every evaluation ends.

mod common;

use common::*;
use toyos_aml::{Error, Interpreter, Value};

const ZERO: &[u8] = &[0x00];

fn load(body: &[u8]) -> Result<(), Error> {
    Interpreter::new().load_bytes(&mut Machine::default(), &dsdt(body))
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
    // A MultiNamePath of zero segments names what the NullName does: no
    // object for Name to define.
    assert!(matches!(load(&cat(&[&[0x08, 0x2F, 0x00], &int(1)])), Err(Error::Rule(_))));
    // A byte above AsciiChar's range ends nothing and is kept, by the owner's
    // ruling to refuse only what is truly malformed; a string without its
    // NullChar is.
    let (mut i, mut m) = loaded(&def_name("S", &[0x0D, b'a', 0x80, 0x00]));
    assert_eq!(i.evaluate(&mut m, "\\S", &[]), Ok(Value::String(vec![b'a', 0x80])));
    assert!(matches!(load(&def_name("S", &[0x0D, b'a'])), Err(Error::Malformed { .. })));
    // A multi-byte PkgLength's reserved bits 5-4 change nothing.
    assert!(load(&cat(&[&[0x10, 0x76, 0x00], &name("\\"), &[0xA3; 99]])).is_ok());
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
                            "ISAB",
                            &cat(&[
                                &def_name("_ADR", &int(0x001F_0000)),
                                &op_region("ISAR", 0x02, &int(0x40), &int(0x10)),
                                &field("ISAR", 1, &[unit("R40", 8), unit("R41", 8)]),
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
    let paths = ["\\MAIN", "\\OSI", "\\_S5", "\\_SB.PKG", "\\_SB.BF", "\\_SB.A", "\\_SB.F1", "\\_SB.PCI0.ISAB.R41"];
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let (mut loads, mut values) = (0, 0);
    for _ in 0..20_000 {
        let mut body = seed.clone();
        mutate(&mut body, &mut r);
        let mut m = Machine::default();
        let mut i = Interpreter::new();
        if i.load_bytes(&mut m, &dsdt(&body)).is_err() {
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
        if i.load_bytes(&mut m, &dsdt(&seed[..n])).is_ok() {
            let _ = i.evaluate(&mut m, "\\MAIN", &[Value::Integer(1), Value::Integer(2)]);
        }
    }
}

/// Review round 1, BLOCKER 3: every object an operator constructs is
/// bounded, a conversion's output as much as a literal's.
#[test]
fn a_conversion_is_bounded_in_what_it_constructs() {
    assert!(matches!(returns(&ret(&op1(0x98, &buffer(&int(0x10_0000), &[]), ZERO))), Err(Error::Bound(_))));
    assert!(matches!(returns(&ret(&op1(0x97, &buffer(&int(0x10_0000), &[]), ZERO))), Err(Error::Bound(_))));
    // ToHexString (ToBuffer (...)), twenty deep, stored to Debug and so never copied.
    let mut e = buffer(&int(0x1000), &[]);
    for _ in 0..20 {
        e = op1(0x98, &op1(0x96, &e, ZERO), ZERO);
    }
    assert!(matches!(returns(&store(&e, &debug())), Err(Error::Bound(_))));
}

/// BLOCKER 4: a step's cost is in proportion to the work it does, so one
/// that walks a mebibyte is charged for it.
#[test]
fn work_in_one_step_is_charged_in_proportion() {
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("BIG", &buffer(&int(0x10_0000), &[])),
        &cat(&[&[0x5B, 0x13], &name("BIG"), &int(0), &int(0x80_0000), &name("FLD")]),
        &method("WALK", 0, &while_(&int(1), &store(&name("FLD"), &local(0)))),
        &method("PKGS", 0, &while_(&int(1), &store(&var_package(&int(0x1_0000), &[]), &local(0)))),
        &method("BUFS", 0, &while_(&int(1), &store(&buffer(&int(0x10_0000), &[]), &local(0)))),
    ]));
    for p in ["\\WALK", "\\PKGS", "\\BUFS"] {
        assert!(matches!(i.evaluate(&mut m, p, &[]), Err(Error::Bound(_))), "{p}");
    }
}

/// How often `While (One) { op  Increment (\CNT) }` ran, in a method `scopes`
/// devices down, before the step bound refused it.
fn iterations(setup: &[u8], scopes: usize, op: &[u8]) -> u64 {
    let mut body = method("MAIN", 0, &while_(&int(1), &cat(&[op, &increment(&name("\\CNT"))])));
    for d in (0..scopes).rev() {
        body = device(&format!("N{d:03}"), &body);
    }
    let main: String = (0..scopes).map(|d| format!("N{d:03}.")).collect();
    let (mut i, mut m) = loaded(&cat(&[&def_name("CNT", &int(0)), setup, &body]));
    assert!(matches!(i.evaluate(&mut m, &format!("\\{main}MAIN"), &[]), Err(Error::Bound(_))));
    match i.evaluate(&mut m, "\\CNT", &[]) {
        Ok(Value::Integer(n)) => n,
        other => panic!("{other:?}"),
    }
}

/// The work one step may hide is a table's to size wherever a name is read
/// or walked, wherever an operator reads more of an object than it makes and
/// wherever a definition moves the namespace's arena to make room: each is
/// charged for all of it, so a loop of one gets no further than the step
/// bound over that charge.
#[test]
fn a_walk_or_a_read_a_table_sizes_is_charged_for_all_of_it() {
    const STEPS: u64 = 1 << 20;
    const SCOPES: usize = 200;
    const BYTES: usize = 1 << 14;
    // The arena is full at 1,024 nodes when MAIN runs: the root and the nine
    // predefined names, CNT, these 1,011, MAKE and MAIN. MAKE's name is one
    // more, and its exit gives the slots back; a node is more than a step's
    // worth of bytes to move.
    const NODES: usize = 1024;
    let crowd: Vec<Vec<u8>> = (0..NODES - 13).map(|n| def_name(&format!("X{n:03X}"), &int(0))).collect();
    let crowd = cat(&[&crowd.concat(), &method("MAKE", 0, &def_name("TMP", &int(0)))]);
    let cond_ref_of = |n: &[u8]| cat(&[&[0x5B, 0x12], n, &local(0)]);
    let deep: Vec<String> = (0..SCOPES).map(|d| format!("N{d:03}")).collect();
    let deep = format!("\\{}", deep.join("."));
    let chain = (0..SCOPES).rev().fold(Vec::new(), |inner, d| device(&format!("N{d:03}"), &inner));
    // A string an operator makes, which Name does not take: made at load.
    let made = |text: &[u8]| {
        let to_string = cat(&[&[0x9C], &buffer(&int(text.len() as u64), text), &ones(), ZERO]);
        cat(&[&def_name("BIG", &string("")), &store(&to_string, &name("BIG"))])
    };
    let big = def_name("BIG", &buffer(&int(BYTES as u64), &[]));
    let small = def_name("SMAL", &buffer(&int(1), &[]));
    let per_scope = STEPS / SCOPES as u64;
    let per_byte = STEPS / (BYTES / 64) as u64;
    let cases: Vec<(&str, u64, u64)> = vec![
        ("a lone name that is nowhere, searched to the root", per_scope, iterations(&[], SCOPES, &cond_ref_of(b"ZZZZ"))),
        ("a path of that many segments", per_scope, iterations(&chain, 0, &cond_ref_of(&name(&deep)))),
        ("a name behind parent prefixes", per_byte, iterations(&[], 0, &cond_ref_of(&cat(&[&vec![b'^'; BYTES], b"ZZZZ"])))),
        ("parent prefixes, each a scope climbed", per_scope, iterations(&[], SCOPES, &cond_ref_of(&cat(&[&[b'^'; SCOPES], b"ZZZZ"])))),
        (
            "a definition by a path of that many segments",
            per_scope,
            iterations(&cat(&[&chain, &method("MAKE", 0, &def_name(&format!("{deep}.TMP"), &int(0)))]), 0, &name("MAKE")),
        ),
        ("a definition that moves the arena, which its method's exit moves back", STEPS / NODES as u64, iterations(&crowd, 0, &name("MAKE"))),
        ("Notify, which names its device by its path", per_scope, iterations(&[], SCOPES, &cat(&[&[0x86], &name("^"), &int(0x80)]))),
        ("a long buffer stored to a short one", per_byte, iterations(&cat(&[&big, &small]), 0, &store(&name("BIG"), &name("SMAL")))),
        (
            "a long buffer stored to a buffer field of a bit",
            per_byte,
            iterations(&cat(&[&big, &small, &[0x8D], &name("SMAL"), &int(0), &name("BIT0")]), 0, &store(&name("BIG"), &name("BIT0"))),
        ),
        ("ToString of a long buffer's first character", per_byte, iterations(&big, 0, &cat(&[&[0x9C], &name("BIG"), &int(1), &local(0)]))),
        ("Mid of a long buffer's first byte", per_byte, iterations(&big, 0, &cat(&[&[0x9E], &name("BIG"), &int(0), &int(1), &local(0)]))),
        ("ToInteger of a long string of zeros", per_byte, iterations(&made(&vec![b'0'; BYTES]), 0, &cat(&[&[0x99], &name("BIG"), &local(0)]))),
        (
            "DerefOf of a long string that names the root",
            per_byte,
            iterations(&made(&cat(&[b"\\", &vec![b'^'; BYTES - 1]])), 0, &store(&deref(&name("BIG")), &local(0))),
        ),
    ];
    let over: Vec<_> = cases.iter().filter(|(_, most, ran)| ran > most).collect();
    assert!(over.is_empty(), "ran more often than the step bound over its charge, as (what, at most, ran): {over:#?}");
    // Each loop did run: the bound is what ended it, not a refusal of its first pass.
    let idle: Vec<_> = cases.iter().filter(|(_, _, ran)| *ran == 0).collect();
    assert!(idle.is_empty(), "{idle:#?}");
}

/// BLOCKER 5: what an interpreter holds live is bounded in sum, not only
/// object by object.
#[test]
fn what_is_held_live_is_bounded_in_sum() {
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("GPKG", &var_package(&int(0x1_0000), &[])),
        &def_name("_S5", &package(&[int(7), int(7), int(0), int(0)])),
        &method("MAKE", 0, &ret(&buffer(&int(0x10_0000), &[]))),
        &method(
            "FILL",
            0,
            &cat(&[
                &store(&int(0), &local(0)),
                &while_(
                    &int(1),
                    &cat(&[&store(&buffer(&int(0x10_0000), &[]), &index(&name("GPKG"), &local(0), ZERO)), &increment(&local(0))]),
                ),
            ]),
        ),
    ]));
    assert_eq!(i.evaluate(&mut m, "\\MAKE", &[]).map(|v| v == Value::Buffer(vec![0; 0x10_0000])), Ok(true));
    assert!(matches!(i.evaluate(&mut m, "\\FILL", &[]), Err(Error::Bound(_))));
    // What the refused evaluation stored stays held: the interpreter goes on
    // answering what needs nothing new held, a named package among it, and
    // refuses what does.
    let seven = Value::Integer(7);
    let zero = Value::Integer(0);
    assert_eq!(i.evaluate(&mut m, "\\_S5", &[]), Ok(Value::Package(vec![seven.clone(), seven, zero.clone(), zero])));
    assert!(matches!(i.evaluate(&mut m, "\\MAKE", &[]), Err(Error::Bound(_))));
}

/// A table a method still runs from and every namespace node count against
/// what an interpreter holds live, as its strings, buffers and packages do.
#[test]
fn tables_and_names_are_held_against_the_live_bound() {
    // Tables of a mebibyte, each kept whole by the one method it defines.
    let (mut i, mut m) = loaded(&[]);
    let kept = |n: usize| table(b"SSDT", 2, &method(&format!("M{n:03}"), 0, &vec![0xA3; (1 << 20) - 52]));
    let refused = (0..17).find_map(|n| i.load_bytes(&mut m, &kept(n)).err().map(|e| (n, e)));
    let Some((n, Error::Bound(_))) = refused else { panic!("seventeen mebibytes of tables are held: {refused:?}") };
    assert!(matches!(i.evaluate(&mut m, &format!("\\M{n:03}"), &[]), Err(Error::NotFound(_))));
    assert_eq!(i.evaluate(&mut m, "\\_REV", &[]), Ok(Value::Integer(2)));

    // Field units, five bytes of table each: 204,000 names a table.
    let digits = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let units: Vec<Vec<u8>> = (0..4000)
        .map(|u| unit(std::str::from_utf8(&[b'A' + (u / 1296) as u8, digits[u / 36 % 36], digits[u % 36]]).unwrap(), 8))
        .collect();
    let dense = |t: usize| {
        let devices: Vec<Vec<u8>> = (0..51).map(|d| device(&format!("D{t}{d:02}"), &field("\\MEM", 0x01, &units))).collect();
        table(b"SSDT", 2, &devices.concat())
    };
    let (mut i, mut m) = loaded(&op_region("MEM", 0x00, &int(0), &int(0x1000)));
    let refused = (0..3).find_map(|t| i.load_bytes(&mut m, &dense(t)).err().map(|e| (t, e)));
    let Some((t, Error::Bound(_))) = refused else { panic!("612,000 names are held: {refused:?}") };
    assert!(matches!(i.evaluate(&mut m, &format!("\\D{t}00.AAA"), &[]), Err(Error::NotFound(_))));
    assert_eq!(i.evaluate(&mut m, "\\_REV", &[]), Ok(Value::Integer(2)));

    // Package elements naming objects no table defines, each by a path of 255
    // segments: 1,020 a table, which holds no method and so is not kept.
    let long = cat(&[&[b'\\', 0x2F, 255], &b"ZZZZ".repeat(255)]);
    let unresolved = |t: usize| {
        let packages: Vec<Vec<u8>> = (0..4).map(|p| def_name(&format!("P{p}"), &package(&vec![long.clone(); 255]))).collect();
        table(b"SSDT", 2, &device(&format!("L{t:03}"), &packages.concat()))
    };
    let (mut i, mut m) = loaded(&[]);
    let refused = (0..17).find_map(|t| i.load_bytes(&mut m, &unresolved(t)).err().map(|e| (t, e)));
    let Some((t, Error::Bound(_))) = refused else { panic!("seventeen mebibytes of names are held: {refused:?}") };
    assert!(matches!(i.evaluate(&mut m, &format!("\\L{t:03}"), &[]), Err(Error::NotFound(_))));
}

/// A String stored to a field is written a character at a time, each charged
/// as it is written: 98,303 characters into a field of a mebibyte end at the
/// step bound, having made no piece ahead of its write.
#[test]
fn a_string_stored_to_a_field_is_bounded_as_it_is_written() {
    let body = cat(&[
        &op_region("MEM", 0x00, &int(0), &int(0x10_0000)),
        &field("MEM", 0x01, &[unit("HUGE", 0x80_0000)]),
        &method("MAIN", 0, &store(&op1(0x98, &buffer(&int(0x8000), &[]), ZERO), &name("HUGE"))),
    ]);
    let mut i = Interpreter::new();
    i.load_bytes(&mut Machine::default(), &dsdt(&body)).unwrap();
    assert!(matches!(i.evaluate(&mut Sink, "\\MAIN", &[]), Err(Error::Bound(_))));
}

/// A reference to a LocalX or ArgX does not hold it: it reaches it while its
/// method runs, and names nothing once that method has exited, whichever way
/// the reference left.
#[test]
fn a_reference_to_a_local_ends_with_its_method() {
    let give = method("GIVE", 0, &ret(&ref_of(&local(0))));
    let put = method("PUT", 1, &store(&ref_of(&local(1)), &arg(0)));
    let set = method("SET", 1, &store(&int(5), &arg(0)));
    // Each GIVE hands back a LocalX of a frame that is gone; storing through
    // it would chain them.
    let chain = while_(
        &int(1),
        &cat(&[&store(&name("GIVE"), &local(2)), &store(&local(0), &deref(&local(2))), &store(&local(2), &local(0))]),
    );
    // Storing it through itself would make it hold itself.
    let cycle = cat(&[&store(&name("GIVE"), &local(2)), &store(&local(2), &deref(&local(2)))]);
    // A callee's LocalX, left behind through an ArgX that refers to the caller's.
    let through = cat(&[&cat(&[&name("PUT"), &ref_of(&local(2))]), &ret(&deref(&local(2)))]);
    let live = cat(&[
        &store(&ref_of(&local(0)), &local(1)),
        &store(&int(2), &deref(&local(1))),
        &cat(&[&name("SET"), &ref_of(&local(3))]),
        &ret(&add(&local(0), &local(3), ZERO)),
    ]);
    let (mut i, mut m) = loaded(&cat(&[
        &give,
        &put,
        &set,
        &method("CHAN", 0, &chain),
        &method("CYCL", 0, &cycle),
        &method("THRU", 0, &through),
        &method("LIVE", 0, &live),
    ]));
    for p in ["\\CHAN", "\\CYCL", "\\THRU"] {
        assert!(matches!(i.evaluate(&mut m, p, &[]), Err(Error::NotFound(_))), "{p}");
    }
    assert_eq!(i.evaluate(&mut m, "\\LIVE", &[]), Ok(Value::Integer(7)));
}

/// BLOCKER 6: a reference to a package element or to a LocalX or ArgX never
/// enters a package or a named object, so no chain of them forms and no
/// cycle outlives its evaluation.
#[test]
fn a_reference_chain_cannot_form() {
    let chain = while_(
        &int(1),
        &cat(&[
            &store(&package(&[int(0)]), &local(1)),
            &store(&local(0), &index(&local(1), &int(0), ZERO)),
            &store(&index(&local(1), &int(0), ZERO), &local(0)),
        ]),
    );
    let seeded = cat(&[&store(&index(&package(&[int(0)]), &int(0), ZERO), &local(0)), &chain]);
    assert!(matches!(returns(&seeded), Err(Error::Type(_))));
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("NUM", &int(0)),
        &method("NAME", 0, &copy_object(&ref_of(&local(0)), &name("NUM"))),
        &method("SELF", 0, &cat(&[&store(&package(&[int(0)]), &local(0)), &store(&index(&local(0), &int(0), ZERO), &index(&local(0), &int(0), ZERO))])),
    ]));
    assert!(matches!(i.evaluate(&mut m, "\\NAME", &[]), Err(Error::Type(_))));
    assert!(matches!(i.evaluate(&mut m, "\\SELF", &[]), Err(Error::Type(_))));
}

/// BLOCKER 9 (a) and (b): a Wait that times out and a Stall are charged to
/// the evaluation's time, as a Sleep is.
#[test]
fn waits_and_stalls_are_bounded_in_time_asked() {
    let event = cat(&[&[0x5B, 0x02], &name("EVT")]);
    let wait = cat(&[&[0x5B, 0x25], &name("EVT"), &int(0xFFFE)]);
    let (mut i, mut m) = loaded(&cat(&[
        &event,
        &method("WAIT", 0, &while_(&int(1), &wait)),
        &method("STAL", 0, &while_(&int(1), &cat(&[&[0x5B, 0x21], &int(0xFF)]))),
    ]));
    assert!(matches!(i.evaluate(&mut m, "\\WAIT", &[]), Err(Error::Bound(_))));
    let waited: u64 = m.log.iter().map(|e| if let Event::Sleep(ms) = e { *ms * 1000 } else { 0 }).sum();
    assert!(waited <= 10_000_000, "{waited} µs");
    m.log.clear();
    assert!(matches!(i.evaluate(&mut m, "\\STAL", &[]), Err(Error::Bound(_))));
    let stalled: u64 = m.log.iter().map(|e| if let Event::Stall(us) = e { *us } else { 0 }).sum();
    assert!(stalled <= 10_000_000, "{stalled} µs");
}

fn sleep(ms: u64) -> Vec<u8> {
    cat(&[&[0x5B, 0x22], &int(ms)])
}

fn stall(us: u64) -> Vec<u8> {
    cat(&[&[0x5B, 0x21], &int(us)])
}

/// The limit is the most an evaluation may ask, to the microsecond: the
/// caller's where it names one, and ten seconds where it does not, for a
/// load too. What crosses it is not asked of the host.
#[test]
fn the_wait_limit_is_exact_and_the_callers_to_name() {
    let (mut i, mut m) = loaded(&cat(&[
        &method("TEN0", 0, &cat(&[&sleep(9_999), &stall(250), &stall(250), &stall(250), &stall(250), &ret(&int(1))])),
        &method("TEN1", 0, &cat(&[&sleep(10_000), &stall(1), &ret(&int(1))])),
        &method("TWEN", 0, &cat(&[&sleep(20_000), &ret(&int(2))])),
        &method("TWE1", 0, &cat(&[&sleep(20_000), &stall(1), &ret(&int(2))])),
        &method("NONE", 0, &cat(&[&sleep(0), &stall(0), &ret(&int(3))])),
        &method("ONE0", 0, &cat(&[&stall(1), &ret(&int(3))])),
    ]));
    let asleep = Error::Bound("more time asleep than one evaluation may spend");
    let over = Err(asleep.clone());
    assert_eq!(toyos_aml::MAX_WAIT_US, 10_000_000);

    assert_eq!(i.evaluate(&mut m, "\\TEN0", &[]), Ok(Value::Integer(1)));
    assert_eq!(i.usage().waited_us, 10_000_000);
    m.log.clear();
    assert_eq!(i.evaluate(&mut m, "\\TEN1", &[]), over);
    assert_eq!(i.usage().waited_us, 10_000_001);
    assert_eq!(m.log, vec![Event::Sleep(10_000)]);
    assert_eq!(i.evaluate(&mut m, "\\TWEN", &[]), over);

    assert_eq!(i.evaluate_within(&mut m, "\\TWEN", &[], 20_000_000), Ok(Value::Integer(2)));
    assert_eq!(i.usage().waited_us, 20_000_000);
    assert_eq!(i.evaluate_within(&mut m, "\\TWE1", &[], 20_000_000), over);
    assert_eq!(i.evaluate_within(&mut m, "\\TEN0", &[], 9_999_999), over);
    assert_eq!(i.evaluate_within(&mut m, "\\NONE", &[], 0), Ok(Value::Integer(3)));
    assert_eq!(i.evaluate_within(&mut m, "\\ONE0", &[], 0), over);

    let mut m = Machine::default();
    assert_eq!(Interpreter::new().load_bytes(&mut m, &dsdt(&cat(&[&sleep(10_000), &def_name("A", &int(1))]))), Ok(()));
    assert_eq!(Interpreter::new().load_bytes(&mut m, &dsdt(&cat(&[&sleep(10_000), &stall(1)]))), Err(asleep));
}

/// What a load or an evaluation took is read after it, a refused one's too,
/// and is that call's alone.
#[test]
fn usage_is_each_calls_own_and_a_refusals_too() {
    let mut m = Machine::default();
    let mut i = Interpreter::new();
    let empty = i.usage();
    assert_eq!((empty.steps, empty.waited_us), (0, 0));
    assert!(empty.live > 0 && empty.live < 4096, "{empty:?}");

    let body = cat(&[
        &def_name("PKG0", &package(&[int(0)])),
        &method("SPIN", 0, &while_(&int(1), &[0xA3])),
        &method("NAP0", 0, &cat(&[&sleep(3), &stall(5)])),
        &method("KEEP", 0, &store(&buffer(&int(1000), &[]), &index(&name("PKG0"), &int(0), ZERO))),
        &method("DROP", 0, &store(&int(0), &index(&name("PKG0"), &int(0), ZERO))),
        &method("RETB", 0, &ret(&buffer(&int(1000), &[]))),
    ]);
    i.load_bytes(&mut m, &dsdt(&body)).unwrap();
    let loaded = i.usage();
    // Six terms and what they hold: a load is counted as an evaluation is.
    assert!(loaded.steps >= 6 && loaded.steps < 64, "{loaded:?}");
    assert_eq!(loaded.waited_us, 0);
    assert!(loaded.live > empty.live + body.len(), "{loaded:?}");

    // The step bound's refusal has taken the bound and the step it refused.
    assert!(matches!(i.evaluate(&mut m, "\\SPIN", &[]), Err(Error::Bound(_))));
    assert_eq!(i.usage(), toyos_aml::Usage { steps: (1 << 20) + 1, waited_us: 0, live: loaded.live });

    i.evaluate(&mut m, "\\NAP0", &[]).unwrap();
    let napped = i.usage();
    assert_eq!((napped.waited_us, napped.live), (3_005, loaded.live));
    assert!(napped.steps >= 4 && napped.steps < 16, "{napped:?}");

    // What an evaluation leaves stored is held after it; what it hands its
    // caller is not.
    i.evaluate(&mut m, "\\KEEP", &[]).unwrap();
    let kept = i.usage();
    assert!(kept.live >= loaded.live + 1000 && kept.live < loaded.live + 1100, "{kept:?}");
    assert_eq!(kept.waited_us, 0);
    assert_eq!(i.evaluate(&mut m, "\\RETB", &[]), Ok(Value::Buffer(vec![0; 1000])));
    assert_eq!(i.usage().live, kept.live);
    i.evaluate(&mut m, "\\DROP", &[]).unwrap();
    assert_eq!(i.usage().live, loaded.live);

    // A refusal before anything ran took nothing.
    assert!(matches!(i.evaluate(&mut m, "\\NONE", &[]), Err(Error::NotFound(_))));
    assert_eq!(i.usage(), toyos_aml::Usage { steps: 0, waited_us: 0, live: loaded.live });

    // A refused load took its steps, and holds what was held before it.
    let refused = table(b"SSDT", 2, &cat(&[&def_name("\\NEW0", &buffer(&int(5000), &[])), &sleep(7), &def_name("\\PKG0", &int(1))]));
    assert!(matches!(i.load_bytes(&mut m, &refused), Err(Error::Exists(_))));
    let after = i.usage();
    assert_eq!((after.waited_us, after.live), (7_000, loaded.live));
    assert!(after.steps >= 3 && after.steps < 256, "{after:?}");
}

/// BLOCKER 9 (c): a reference to an object a method created names nothing
/// once the method exits, even after its slot is reused.
#[test]
fn a_reference_outliving_its_object_names_nothing() {
    // MAIN's own Name reuses TMP's slot and is alive when the old
    // reference is followed.
    let (mut i, mut m) = loaded(&cat(&[
        &method("MAKE", 0, &cat(&[&def_name("TMP", &int(7)), &ret(&ref_of(&name("TMP")))])),
        &method(
            "MAIN",
            0,
            &cat(&[&store(&name("MAKE"), &local(0)), &def_name("OTHR", &int(9)), &ret(&deref(&local(0)))]),
        ),
    ]));
    assert!(matches!(i.evaluate(&mut m, "\\MAIN", &[]), Err(Error::NotFound(_))));
}

/// BLOCKER 9 (d): ConcatenateResTemplate is bounded in what it constructs.
#[test]
fn a_resource_template_join_is_bounded() {
    let big = buffer(&int(0x10_0000), &[]);
    let r = returns(&ret(&op2(0x84, &op2(0x73, &big, &buffer(&int(2), &[0x79, 0]), ZERO), &op2(0x73, &big, &buffer(&int(2), &[0x79, 0]), ZERO), ZERO)));
    assert!(matches!(r, Err(Error::Bound(_))));
    let half = buffer(&int(0x8_0000), &[]);
    let tail = buffer(&int(2), &[0x79, 0]);
    let r = returns(&ret(&op2(0x84, &op2(0x73, &half, &tail, ZERO), &op2(0x73, &half, &tail, ZERO), ZERO)));
    assert!(matches!(r, Err(Error::Bound(_))));
}

/// BLOCKER 7: an Alias is an object its table or method created like any
/// other.
#[test]
fn an_alias_goes_with_what_created_it() {
    let alias = cat(&[&[0x06], &name("\\SRC"), &name("ALI")]);
    let (mut i, mut m) = loaded(&cat(&[&def_name("SRC", &int(1)), &method("M", 0, &alias)]));
    assert_eq!(i.evaluate(&mut m, "\\M", &[]), Ok(Value::Uninitialized));
    assert_eq!(i.evaluate(&mut m, "\\M", &[]), Ok(Value::Uninitialized));
    let ssdt = table(b"SSDT", 2, &cat(&[&[0x06], &name("\\SRC"), &name("\\B"), &def_name("\\SRC", &int(2))]));
    assert!(matches!(i.load_bytes(&mut m, &ssdt), Err(Error::Exists(_))));
    assert!(matches!(i.evaluate(&mut m, "\\B", &[]), Err(Error::NotFound(_))));
}
