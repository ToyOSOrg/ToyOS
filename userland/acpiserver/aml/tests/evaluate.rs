//! Control methods evaluated (§5.5.2, §19.3.5, §19.6, §20.2.5.3-4).

mod common;

use common::*;
use toyos_aml::{Error, Value};

const ZERO: &[u8] = &[0x00];

fn i(v: u64) -> Result<Value, Error> {
    Ok(Value::Integer(v))
}

fn b(v: &[u8]) -> Result<Value, Error> {
    Ok(Value::Buffer(v.to_vec()))
}

fn st(v: &str) -> Result<Value, Error> {
    Ok(s(v))
}

const ONES: u64 = u64::MAX;

#[test]
fn integer_arithmetic_wraps_at_the_integer_width() {
    assert_eq!(returns(&ret(&add(&int(u64::MAX), &int(2), ZERO))), i(1));
    assert_eq!(returns(&ret(&op2(0x74, &int(1), &int(2), ZERO))), i(ONES));
    assert_eq!(returns(&ret(&op2(0x77, &int(6), &int(7), ZERO))), i(42));
    assert_eq!(returns(&ret(&op2(0x79, &int(1), &int(63), ZERO))), i(1 << 63));
    assert_eq!(returns(&ret(&op2(0x79, &int(1), &int(64), ZERO))), i(0));
    assert_eq!(returns(&ret(&op2(0x7A, &int(0x80), &int(4), ZERO))), i(8));
    assert_eq!(returns(&ret(&op2(0x7A, &int(0x80), &int(200), ZERO))), i(0));
    assert_eq!(returns(&ret(&op2(0x7B, &int(0xF0), &int(0x3C), ZERO))), i(0x30));
    assert_eq!(returns(&ret(&op2(0x7C, &int(0xF0), &int(0x3C), ZERO))), i(!0x30));
    assert_eq!(returns(&ret(&op2(0x7D, &int(0xF0), &int(0x0F), ZERO))), i(0xFF));
    assert_eq!(returns(&ret(&op2(0x7E, &int(0xF0), &int(0x0F), ZERO))), i(!0xFF));
    assert_eq!(returns(&ret(&op2(0x7F, &int(0xFF), &int(0x0F), ZERO))), i(0xF0));
    assert_eq!(returns(&ret(&op2(0x85, &int(17), &int(5), ZERO))), i(2));
    assert_eq!(returns(&ret(&op1(0x80, &int(0), ZERO))), i(ONES));
    // FindSetLeftBit and FindSetRightBit: one-based, zero for none (§19.6.48-49).
    assert_eq!(returns(&ret(&op1(0x81, &int(0x50), ZERO))), i(7));
    assert_eq!(returns(&ret(&op1(0x82, &int(0x50), ZERO))), i(5));
    assert_eq!(returns(&ret(&op1(0x81, &int(0), ZERO))), i(0));
    assert_eq!(returns(&ret(&op1(0x82, &int(0), ZERO))), i(0));
}

#[test]
fn divide_stores_remainder_and_quotient_and_refuses_zero() {
    let body = cat(&[&[0x78], &int(17), &int(5), &local(0), &local(1), &ret(&package_of_locals())]);
    fn package_of_locals() -> Vec<u8> {
        add(&op2(0x77, &local(1), &int(0x100), ZERO), &local(0), ZERO)
    }
    assert_eq!(returns(&body), i(0x302));
    assert_eq!(returns(&ret(&cat(&[&[0x78], &int(1), &int(0), ZERO, ZERO]))), Err(Error::Rule("Divide by zero (§19.6.32)")));
    assert_eq!(returns(&ret(&op2(0x85, &int(1), &int(0), ZERO))), Err(Error::Rule("Mod by zero (§19.6.86)")));
}

#[test]
fn bcd_converts_both_ways_and_refuses_what_is_not_bcd() {
    assert_eq!(returns(&ret(&cat(&[&[0x5B, 0x29], &int(1234), ZERO]))), i(0x1234));
    assert_eq!(returns(&ret(&cat(&[&[0x5B, 0x28], &int(0x1234), ZERO]))), i(1234));
    assert!(matches!(returns(&ret(&cat(&[&[0x5B, 0x28], &int(0x1A), ZERO]))), Err(Error::Rule(_))));
}

#[test]
fn logical_operators_answer_ones_or_zero() {
    assert_eq!(returns(&ret(&cat(&[&[0x90], &int(1), &int(2)]))), i(ONES));
    assert_eq!(returns(&ret(&cat(&[&[0x90], &int(1), &int(0)]))), i(0));
    assert_eq!(returns(&ret(&cat(&[&[0x91], &int(0), &int(2)]))), i(ONES));
    assert_eq!(returns(&ret(&lnot(&int(0)))), i(ONES));
    // LNotEqual, LLessEqual and LGreaterEqual are LNot of the others (§20.2.5.4).
    assert_eq!(returns(&ret(&lnot(&lequal(&int(1), &int(2))))), i(ONES));
    assert_eq!(returns(&ret(&lnot(&lgreater(&int(2), &int(2))))), i(ONES));
    assert_eq!(returns(&ret(&lnot(&lless(&int(3), &int(2))))), i(ONES));
}

/// §19.6.69-72: the first operand's type decides; strings and buffers
/// compare byte by byte, an equal shorter one the lesser.
#[test]
fn comparisons_follow_the_first_operands_type() {
    assert_eq!(returns(&ret(&lequal(&string("ABC"), &string("ABC")))), i(ONES));
    assert_eq!(returns(&ret(&lless(&string("AB"), &string("ABC")))), i(ONES));
    assert_eq!(returns(&ret(&lgreater(&string("B"), &string("ABC")))), i(ONES));
    assert_eq!(returns(&ret(&lequal(&int(0x1234), &string("1234")))), i(ONES));
    assert_eq!(returns(&ret(&lequal(&string("00000012"), &int(0x12)))), i(0));
    assert_eq!(returns(&ret(&lequal(&string("0000000000000012"), &int(0x12)))), i(ONES));
    assert_eq!(returns(&ret(&lequal(&buffer(&int(2), &[1, 2]), &int(0x0201)))), i(0));
    assert_eq!(returns(&ret(&lequal(&buffer(&int(8), &[1, 2]), &int(0x0201)))), i(ONES));
    assert!(matches!(returns(&ret(&lequal(&package(&[]), &int(0)))), Err(Error::Type(_))));
}

#[test]
fn if_else_and_while_with_break_and_continue() {
    let count = cat(&[
        &store(&int(0), &local(0)),
        &store(&int(0), &local(1)),
        &while_(
            &lless(&local(0), &int(10)),
            &cat(&[
                &increment(&local(0)),
                &if_(&lequal(&local(0), &int(3)), &[0x9F]),
                &if_(&lequal(&local(0), &int(8)), &[0xA5]),
                &add(&local(1), &local(0), &local(1)),
            ]),
        ),
        &ret(&local(1)),
    ]);
    // 1+2+4+5+6+7: 3 continued past, 8 broke out.
    assert_eq!(returns(&count), i(25));
    let branch = |v: u64| {
        cat(&[
            &if_(&lequal(&int(v), &int(1)), &ret(&string("one"))),
            &else_(&cat(&[&if_(&lequal(&int(v), &int(2)), &ret(&string("two"))), &else_(&ret(&string("other")))])),
        ])
    };
    assert_eq!(returns(&branch(1)), st("one"));
    assert_eq!(returns(&branch(2)), st("two"));
    assert_eq!(returns(&branch(3)), st("other"));
}

#[test]
fn break_and_continue_outside_a_while_are_refused() {
    assert!(matches!(returns(&[0xA5]), Err(Error::Rule(_))));
    assert!(matches!(returns(&if_(&int(1), &[0x9F])), Err(Error::Rule(_))));
}

#[test]
fn methods_take_their_declared_arguments_and_return_a_copy() {
    let (mut ip, mut m) = loaded(&cat(&[
        &method("SUM3", 3, &ret(&add(&add(&arg(0), &arg(1), ZERO), &arg(2), ZERO))),
        &method("MAIN", 0, &ret(&cat(&[&name("SUM3"), &int(1), &int(2), &int(3)]))),
        &def_name("BUF", &buffer(&int(2), &[7, 8])),
        &method("SAME", 0, &ret(&name("BUF"))),
        &method("POKE", 0, &cat(&[&store(&name("SAME"), &local(0)), &store(&int(9), &index(&local(0), &int(0), ZERO))])),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\MAIN", &[]), i(6));
    ip.evaluate(&mut m, "\\POKE", &[]).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\BUF", &[]), b(&[7, 8]));
}

#[test]
fn a_method_without_return_returns_nothing_usable() {
    assert_eq!(returns(&[0xA3]), Ok(Value::Uninitialized));
    let (mut ip, mut m) = loaded(&cat(&[&method("NONE", 0, &[]), &method("MAIN", 0, &ret(&add(&name("NONE"), &int(1), ZERO)))]));
    assert!(matches!(ip.evaluate(&mut m, "\\MAIN", &[]), Err(Error::Type(_))));
}

#[test]
fn recursion_runs_until_its_bound() {
    let fact = method(
        "FACT",
        1,
        &cat(&[
            &if_(&lless(&arg(0), &int(2)), &ret(&int(1))),
            &ret(&op2(0x77, &arg(0), &cat(&[&name("FACT"), &op2(0x74, &arg(0), &int(1), ZERO)]), ZERO)),
        ]),
    );
    let (mut ip, mut m) = loaded(&fact);
    assert_eq!(ip.evaluate(&mut m, "\\FACT", &[Value::Integer(10)]), i(3_628_800));
    assert!(matches!(ip.evaluate(&mut m, "\\FACT", &[Value::Integer(100_000)]), Err(Error::Bound(_))));
}

/// Table 19.7, each conversion an operator applies to a source operand.
#[test]
fn source_operands_convert_by_the_conversion_rules() {
    // String to Integer: hexadecimal up to the first non-digit.
    assert_eq!(returns(&ret(&add(&string("1F"), &int(1), ZERO))), i(0x20));
    assert_eq!(returns(&ret(&add(&string("12G4"), &int(0), ZERO))), i(0x12));
    assert!(matches!(returns(&ret(&add(&string(""), &int(0), ZERO))), Err(Error::Type(_))));
    // Buffer to Integer: least significant byte first, up to eight.
    assert_eq!(returns(&ret(&add(&buffer(&int(2), &[0x34, 0x12]), &int(0), ZERO))), i(0x1234));
    assert_eq!(returns(&ret(&add(&buffer(&int(9), &[1, 0, 0, 0, 0, 0, 0, 0, 9]), &int(0), ZERO))), i(1));
    assert!(matches!(returns(&ret(&add(&buffer(&int(0), &[]), &int(0), ZERO))), Err(Error::Type(_))));
    // A package converts to no integer, nor does an uninitialized local.
    assert!(matches!(returns(&ret(&add(&package(&[]), &int(0), ZERO))), Err(Error::Type(_))));
    assert!(matches!(returns(&ret(&add(&local(3), &int(0), ZERO))), Err(Error::Type(_))));
}

#[test]
fn explicit_conversions_follow_their_definitions() {
    let conv = |op: u8, v: &[u8]| returns(&ret(&cat(&[&[op], v, ZERO])));
    assert_eq!(conv(0x96, &int(0x0102)), b(&[2, 1, 0, 0, 0, 0, 0, 0]));
    assert_eq!(conv(0x96, &string("AB")), b(&[0x41, 0x42, 0]));
    assert_eq!(conv(0x96, &string("")), b(&[]));
    assert_eq!(conv(0x97, &int(1234)), st("1234"));
    assert_eq!(conv(0x97, &buffer(&int(3), &[1, 20, 255])), st("1,20,255"));
    assert_eq!(conv(0x98, &int(0xAB)), st("00000000000000AB"));
    assert_eq!(conv(0x98, &buffer(&int(2), &[0xAB, 0x01])), st("AB,01"));
    assert_eq!(conv(0x99, &string("0x1F")), i(0x1F));
    assert_eq!(conv(0x99, &string("123")), i(123));
    assert!(matches!(conv(0x99, &string("12z")), Err(Error::Type(_))));
    assert!(matches!(conv(0x99, &string("99999999999999999999")), Err(Error::Rule(_))));
    assert_eq!(conv(0x99, &buffer(&int(2), &[0x34, 0x12])), i(0x1234));
    // ToString: up to Length bytes or a NUL (§19.6.141).
    assert_eq!(returns(&ret(&cat(&[&[0x9C], &buffer(&int(5), b"ab\0cd"), &ones(), ZERO]))), st("ab"));
    assert_eq!(returns(&ret(&cat(&[&[0x9C], &buffer(&int(4), b"abcd"), &int(3), ZERO]))), st("abc"));
    // Buffer to String through Concatenate's second operand (Table 19.7).
    assert_eq!(returns(&ret(&op2(0x73, &string(">"), &buffer(&int(2), &[0x0A, 0xFF]), ZERO))), st(">0A FF"));
}

/// Table 19.30, and the example of §19.3.5.4.
#[test]
fn concatenate_takes_the_first_operands_type() {
    assert_eq!(returns(&ret(&op2(0x73, &int(1), &string("2"), ZERO))), b(&[1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]));
    assert_eq!(returns(&ret(&op2(0x73, &string("AB"), &int(0x1F), ZERO))), st("AB000000000000001F"));
    assert_eq!(returns(&ret(&op2(0x73, &buffer(&int(1), &[1]), &string("A"), ZERO))), b(&[1, 0x41, 0]));
    let (mut ip, mut m) = loaded(&cat(&[
        &device("DEVX", &[]),
        &method("MAIN", 0, &ret(&op2(0x73, &string("My Object: "), &name("DEVX"), ZERO))),
        &def_name("ABCD", &buffer(&int(10), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 0])),
        &cat(&[&[0x8A], &name("ABCD"), &int(2), &name("XYZ")]),
        &def_name("MNOP", &string("1234")),
        &method("EXAM", 0, &ret(&op2(0x73, &name("XYZ"), &name("MNOP"), ZERO))),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\MAIN", &[]), st("My Object: [Device]"));
    // §19.3.5.4: both convert to Integers and join as a Buffer. Its text
    // gives XYZ as 0x05040302, the DWord at byte index 1; §19.6.20 defines
    // the field at byte index 2, which holds 3, 4, 5, 6.
    assert_eq!(ip.evaluate(&mut m, "\\EXAM", &[]), b(&[3, 4, 5, 6, 0, 0, 0, 0, 0x34, 0x12, 0, 0, 0, 0, 0, 0]));
}

#[test]
fn mid_takes_what_lies_within() {
    let mid = |src: &[u8], at: u64, n: u64| returns(&ret(&cat(&[&[0x9E], src, &int(at), &int(n), ZERO])));
    assert_eq!(mid(&string("ABCDEF"), 1, 3), st("BCD"));
    assert_eq!(mid(&string("ABCDEF"), 4, 100), st("EF"));
    assert_eq!(mid(&string("ABCDEF"), 10, 1), st(""));
    assert_eq!(mid(&buffer(&int(3), &[1, 2, 3]), 1, 1), b(&[2]));
}

/// ConcatenateResTemplate (§19.6.13), with the End Tag of §6.4.2.9.
#[test]
fn resource_templates_join_under_one_end_tag() {
    let irq = buffer(&int(5), &[0x22, 0x10, 0x00, 0x79, 0x00]);
    let io = buffer(&int(2), &[0x79, 0x00]);
    let r = returns(&ret(&op2(0x84, &irq, &io, ZERO))).unwrap();
    let Value::Buffer(v) = r else { panic!("{r:?}") };
    assert_eq!(&v[..4], &[0x22, 0x10, 0x00, 0x79]);
    assert_eq!(v.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0);
    assert!(matches!(returns(&ret(&op2(0x84, &buffer(&int(1), &[0x79]), &io, ZERO))), Err(Error::Rule(_))));
}

/// Table 19.8 and §19.3.5.8: a store converts to a named object's type, and
/// a named buffer keeps its size; CopyObject takes the source's type.
#[test]
fn store_converts_to_the_named_type_and_copy_object_does_not() {
    let (mut ip, mut m) = loaded(&cat(&[
        &def_name("NUM", &int(0)),
        &def_name("STR", &string("x")),
        &def_name("BUF", &buffer(&int(4), &[])),
        &def_name("PKG", &package(&[int(1)])),
        &method(
            "MAIN",
            0,
            &cat(&[
                &store(&string("1F"), &name("NUM")),
                &store(&int(0xAB), &name("STR")),
                &store(&string("ABCDEF"), &name("BUF")),
                &store(&package(&[int(2), int(3)]), &name("PKG")),
            ]),
        ),
        &method("CPY", 0, &copy_object(&string("now a string"), &name("NUM"))),
        &method("BAD", 0, &store(&int(1), &name("PKG"))),
    ]));
    ip.evaluate(&mut m, "\\MAIN", &[]).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\NUM", &[]), i(0x1F));
    assert_eq!(ip.evaluate(&mut m, "\\STR", &[]), st("00000000000000AB"));
    assert_eq!(ip.evaluate(&mut m, "\\BUF", &[]), b(b"ABCD"));
    assert_eq!(ip.evaluate(&mut m, "\\PKG", &[]), Ok(Value::Package(vec![Value::Integer(2), Value::Integer(3)])));
    ip.evaluate(&mut m, "\\CPY", &[]).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\NUM", &[]), st("now a string"));
    assert!(matches!(ip.evaluate(&mut m, "\\BAD", &[]), Err(Error::Type(_))));
}

/// §5.5.2.2 and Table 19.10: a store to an ArgX replaces it, unless it holds
/// a reference, which it stores through.
#[test]
fn an_arg_holding_a_reference_stores_through_it() {
    let (mut ip, mut m) = loaded(&cat(&[
        &def_name("OBJ", &int(1)),
        &method("SET", 1, &store(&int(5), &arg(0))),
        &method("BREF", 0, &cat(&[&name("SET"), &ref_of(&name("OBJ"))])),
        &method("BVAL", 0, &cat(&[&name("SET"), &name("OBJ")])),
    ]));
    ip.evaluate(&mut m, "\\BVAL", &[]).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\OBJ", &[]), i(1));
    ip.evaluate(&mut m, "\\BREF", &[]).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\OBJ", &[]), i(5));
}

/// §19.6.62: Index of a buffer is a byte field, of a package its element.
#[test]
fn index_reaches_into_buffers_strings_and_packages() {
    let (mut ip, mut m) = loaded(&cat(&[
        &def_name("BUFF", &buffer(&int(4), &[1, 2, 3, 4])),
        &def_name("SRCB", &buffer(&int(4), &[0x10, 0x20, 0x30, 0x40])),
        &def_name("STR", &string("ABCDEFGHIJKL")),
        &def_name(
            "IO0D",
            &package(&[package(&[int(1), int(0x3F8), int(0x3F8), int(1), int(8), int(1)]), package(&[int(2)])]),
        ),
        &method(
            "MAIN",
            0,
            &cat(&[
                &store(&int(0x1234_5678), &index(&name("BUFF"), &int(2), ZERO)),
                &store(&name("SRCB"), &index(&name("BUFF"), &int(1), ZERO)),
                &store(&string("ABCDEFGH"), &index(&name("BUFF"), &int(3), ZERO)),
                &store(&string("H"), &index(&name("STR"), &int(2), ZERO)),
                &ret(&deref(&index(&deref(&index(&name("IO0D"), &int(0), ZERO)), &int(5), ZERO))),
            ]),
        ),
        &method("PAST", 0, &ret(&index(&name("BUFF"), &int(4), ZERO))),
        &method("UNIN", 0, &cat(&[&store(&package(&[]), &local(0)), &ret(&deref(&index(&var_package(&int(2), &[]), &int(1), ZERO)))])),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\MAIN", &[]), i(1));
    assert_eq!(ip.evaluate(&mut m, "\\BUFF", &[]), b(&[1, 0x10, 0x78, 0x41]));
    assert_eq!(ip.evaluate(&mut m, "\\STR", &[]), st("ABHDEFGHIJKL"));
    assert!(matches!(ip.evaluate(&mut m, "\\PAST", &[]), Err(Error::Rule(_))));
    assert!(matches!(ip.evaluate(&mut m, "\\UNIN", &[]), Err(Error::Rule(_))));
}

#[test]
fn references_are_made_followed_and_asked_about() {
    let cond = |n: &str| cat(&[&[0x5B, 0x12], &name(n), &local(0)]);
    let (mut ip, mut m) = loaded(&cat(&[
        &def_name("OBJ", &string("abc")),
        &device("DEV", &[]),
        &method(
            "MAIN",
            0,
            &cat(&[
                &store(&ref_of(&name("OBJ")), &local(1)),
                &store(&int(9), &deref(&local(1))),
                &ret(&package(&[name("OBJ")])),
            ]),
        ),
        &method("HAS", 0, &ret(&cond("OBJ"))),
        &method("HASN", 0, &ret(&cond("NOPE"))),
        &method("SIZE", 0, &ret(&size_of(&name("OBJ")))),
        &method("TYPE", 0, &cat(&[&store(&ref_of(&name("DEV")), &local(2)), &ret(&object_type(&local(2)))])),
        &method("OSI", 0, &ret(&cond("\\_OSI"))),
        &method("NAMD", 0, &ret(&deref(&string("\\OBJ")))),
    ]));
    ip.evaluate(&mut m, "\\MAIN", &[]).unwrap();
    // OBJ is a String: the store through the reference converts (§19.3.5.8.3).
    assert_eq!(ip.evaluate(&mut m, "\\OBJ", &[]), st("0000000000000009"));
    assert_eq!(ip.evaluate(&mut m, "\\HAS", &[]), i(ONES));
    assert_eq!(ip.evaluate(&mut m, "\\HASN", &[]), i(0));
    assert_eq!(ip.evaluate(&mut m, "\\SIZE", &[]), i(16));
    assert_eq!(ip.evaluate(&mut m, "\\TYPE", &[]), i(6));
    assert_eq!(ip.evaluate(&mut m, "\\OSI", &[]), i(ONES));
    assert_eq!(ip.evaluate(&mut m, "\\NAMD", &[]), st("0000000000000009"));
}

/// The examples of §19.6.80.
#[test]
fn match_finds_the_first_element_both_tests_accept() {
    let years = package(&[1981, 1983, 1985, 1987, 1989, 1990, 1991, 1993, 1995, 1997, 1999, 2001].map(int));
    let m = |op1: u8, v1: u64, op2: u8, v2: u64, start: u64| {
        returns(&ret(&cat(&[&[0x89], &years, &[op1], &int(v1), &[op2], &int(v2), &int(start)])))
    };
    assert_eq!(m(1, 1993, 0, 0, 0), i(7));
    assert_eq!(m(1, 1984, 0, 0, 0), i(ONES));
    assert_eq!(m(5, 1984, 2, 2000, 0), i(2));
    assert_eq!(m(5, 1984, 2, 2000, 3), i(3));
    assert!(matches!(m(6, 0, 0, 0, 0), Err(Error::Malformed { .. })));
    // An element that does not convert to the MatchObject's type matches nothing.
    let mixed = package(&[string("zz"), int(5)]);
    assert_eq!(returns(&ret(&cat(&[&[0x89], &mixed, &[1], &int(5), &[0], &int(0), &int(0)]))), i(1));
}

#[test]
fn buffers_and_packages_are_sized_as_declared() {
    // §19.6.10: the larger of BufferSize and the initializer.
    assert_eq!(returns(&ret(&buffer(&int(4), &[1, 2]))), b(&[1, 2, 0, 0]));
    assert_eq!(returns(&ret(&buffer(&int(1), &[5, 4, 3]))), b(&[5, 4, 3]));
    // §19.6.101: elements past the initializer are uninitialized.
    assert_eq!(returns(&ret(&var_package(&int(2), &[int(1)]))), Ok(Value::Package(vec![Value::Integer(1), Value::Uninitialized])));
    let over = cat(&[&[0x12], &pkg(&cat(&[&[1], &int(1), &int(2)]))]);
    assert!(matches!(returns(&ret(&over)), Err(Error::Malformed { .. })));
}

#[test]
fn mutexes_follow_sync_levels_and_are_released_by_the_end() {
    let acquire = |n: &str| cat(&[&[0x5B, 0x23], &name(n), &[0xFF, 0xFF]]);
    let release = |n: &str| cat(&[&[0x5B, 0x27], &name(n)]);
    let mutex = |n: &str, level: u8| cat(&[&[0x5B, 0x01], &name(n), &[level]]);
    let (mut ip, mut m) = loaded(&cat(&[
        &mutex("LOW", 1),
        &mutex("HIGH", 5),
        &method("GOOD", 0, &cat(&[&acquire("LOW"), &acquire("HIGH"), &release("HIGH"), &release("LOW"), &ret(&int(1))])),
        &method("DOWN", 0, &cat(&[&acquire("HIGH"), &acquire("LOW")])),
        &method("ORDR", 0, &cat(&[&acquire("LOW"), &acquire("HIGH"), &release("LOW")])),
        &method("KEEP", 0, &acquire("LOW")),
        &method("NONE", 0, &release("LOW")),
        &method("GL", 0, &cat(&[&acquire("\\_GL"), &release("\\_GL")])),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\GOOD", &[]), i(1));
    assert!(matches!(ip.evaluate(&mut m, "\\DOWN", &[]), Err(Error::Rule(_))));
    assert!(matches!(ip.evaluate(&mut m, "\\ORDR", &[]), Err(Error::Rule(_))));
    assert!(matches!(ip.evaluate(&mut m, "\\KEEP", &[]), Err(Error::Rule(_))));
    assert!(matches!(ip.evaluate(&mut m, "\\NONE", &[]), Err(Error::Rule(_))));
    // What an evaluation held is let go: the next one starts clean.
    assert_eq!(ip.evaluate(&mut m, "\\GOOD", &[]), i(1));
    m.log.clear();
    ip.evaluate(&mut m, "\\GL", &[]).unwrap();
    assert_eq!(m.log, vec![Event::GlobalTake(None), Event::GlobalRelease]);
}

/// §19.6.2: an Acquire of `\_GL` waits for the firmware's side at most its
/// TimeoutValue in milliseconds, 0xFFFF naming no bound; one that times out
/// returns True and holds nothing, and one this evaluation already holds asks
/// the host for nothing.
#[test]
fn an_acquire_of_the_global_lock_waits_as_its_timeout_says() {
    let acquire = |ms: u16| cat(&[&[0x5B, 0x23], &name("\\_GL"), &ms.to_le_bytes()]);
    let release = cat(&[&[0x5B, 0x27], &name("\\_GL")]);
    let (mut ip, mut m) = loaded(&cat(&[
        &method("SOON", 0, &cat(&[&store(&acquire(5), &local(0)), &release, &ret(&local(0))])),
        &method("LATE", 0, &ret(&acquire(5))),
        &method("EVER", 0, &cat(&[&store(&acquire(0xFFFF), &local(0)), &store(&acquire(0), &local(1)), &release, &release, &ret(&add(&local(0), &local(1), ZERO))])),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\EVER", &[]), i(0));
    assert_eq!(m.log, vec![Event::GlobalTake(None), Event::GlobalRelease], "the second Acquire asked the host again");
    m.log.clear();
    assert_eq!(ip.evaluate(&mut m, "\\SOON", &[]), i(0));
    assert_eq!(m.log, vec![Event::GlobalTake(Some(5)), Event::GlobalRelease]);
    m.firmware_holds = true;
    m.log.clear();
    assert_eq!(ip.evaluate(&mut m, "\\LATE", &[]), i(ONES), "an Acquire that timed out is True (§19.6.2)");
    assert_eq!(m.log, vec![Event::GlobalTake(Some(5))], "a lock the take did not get was given back");
    m.log.clear();
    assert!(matches!(ip.evaluate(&mut m, "\\EVER", &[]), Err(Error::Host(_))), "an unbounded take came back untaken and the method ran on");
    assert_eq!(m.log, vec![Event::GlobalTake(None)]);
}

#[test]
fn events_count_signals_and_a_wait_without_one_times_out() {
    let event = cat(&[&[0x5B, 0x02], &name("EVT")]);
    let signal = cat(&[&[0x5B, 0x24], &name("EVT")]);
    let wait = |ms: u64| cat(&[&[0x5B, 0x25], &name("EVT"), &int(ms)]);
    let (mut ip, mut m) = loaded(&cat(&[
        &event,
        &method("MAIN", 0, &cat(&[&signal, &store(&wait(5), &local(0)), &ret(&add(&local(0), &wait(5), ZERO))])),
        &method("EVER", 0, &ret(&wait(0xFFFF))),
    ]));
    assert_eq!(ip.evaluate(&mut m, "\\MAIN", &[]), i(ONES));
    assert_eq!(m.log, vec![Event::Sleep(5)]);
    assert!(matches!(ip.evaluate(&mut m, "\\EVER", &[]), Err(Error::Rule(_))));
}

#[test]
fn notify_sleep_stall_and_fatal_reach_the_host() {
    let (mut ip, mut m) = loaded(&cat(&[
        &scope("\\_SB", &device("LID0", &[])),
        &def_name("NUM", &int(0)),
        &method(
            "MAIN",
            0,
            &cat(&[
                &[0x86],
                &name("\\_SB.LID0"),
                &int(0x80),
                &[0x5B, 0x22],
                &int(10),
                &[0x5B, 0x21],
                &int(50),
            ]),
        ),
        &method("NNUM", 0, &cat(&[&[0x86], &name("NUM"), &int(1)])),
        &method("LONG", 0, &cat(&[&[0x5B, 0x21], &int(256)])),
        &method("DIE", 0, &cat(&[&[0x5B, 0x32, 0x01], &0xDEAD_u32.to_le_bytes(), &int(7)])),
    ]));
    ip.evaluate(&mut m, "\\MAIN", &[]).unwrap();
    assert_eq!(m.log, vec![Event::Notify("\\_SB_.LID0".into(), 0x80), Event::Sleep(10), Event::Stall(50)]);
    assert!(matches!(ip.evaluate(&mut m, "\\NNUM", &[]), Err(Error::Type(_))));
    assert!(matches!(ip.evaluate(&mut m, "\\LONG", &[]), Err(Error::Rule(_))));
    assert_eq!(ip.evaluate(&mut m, "\\DIE", &[]), Err(Error::Fatal { kind: 1, code: 0xDEAD, arg: 7 }));
}

/// The owner's ruling, "Like Windows, not Linux": yes to every published
/// Windows version string, no to "Linux" and "FreeBSD".
#[test]
fn osi_answers_like_windows() {
    let osi = |q: &str| returns(&ret(&cat(&[&name("\\_OSI"), &string(q)])));
    assert_eq!(osi("Windows 2022"), i(ONES));
    assert_eq!(osi("Windows 2001 SP1"), i(ONES));
    assert_eq!(osi("Linux"), i(0));
    assert_eq!(osi("FreeBSD"), i(0));
    assert_eq!(osi("Module Device"), i(0));
    assert!(matches!(returns(&ret(&cat(&[&name("\\_OSI"), &int(1)]))), Err(Error::Type(_))));
    // A DSDT of revision 1 answers Ones in 32 bits (§5.7.2).
    let mut m = Machine::default();
    let mut ip = toyos_aml::Interpreter::new();
    ip.load_bytes(&mut m, &table(b"DSDT", 1, &method("Q", 0, &ret(&cat(&[&name("\\_OSI"), &string("Windows 2022")]))))).unwrap();
    assert_eq!(ip.evaluate(&mut m, "\\Q", &[]), i(0xFFFF_FFFF));
}

/// `\_S5`, as the ACPI server's power-off evaluates it: SLP_TYPa and
/// SLP_TYPb first.
#[test]
fn s5_evaluates_to_its_sleep_types() {
    let (mut ip, mut m) = loaded(&cat(&[
        &def_name("SS5", &int(7)),
        &def_name("_S5", &package(&[name("SS5"), int(7), int(0), int(0)])),
    ]));
    assert_eq!(
        ip.evaluate(&mut m, "\\_S5", &[]),
        Ok(Value::Package(vec![Value::Integer(7), Value::Integer(7), Value::Integer(0), Value::Integer(0)]))
    );
}

#[test]
fn the_debug_object_takes_writes_and_refuses_reads() {
    assert_eq!(returns(&cat(&[&store(&int(1), &debug()), &ret(&int(2))])), i(2));
    assert!(matches!(returns(&ret(&debug())), Err(Error::Type(_))));
}
