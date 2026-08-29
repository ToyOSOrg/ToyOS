//! C99 6.4.8: a preprocessing number is one token, held against the grammar,
//! not against the literal shapes the compiler converts. The observable is
//! the preprocessed text — one token prints verbatim, a split one gets the
//! space `tokens_to_string` inserts — and every `is()` expectation was
//! cross-checked against the host `cc -E -P` before it was written down.

mod common;

/// The preprocessed text, line markers suppressed, edges trimmed.
fn pp(source: &str) -> String {
    toyos_cc::preprocess_source(source, "n.c", &common::options(), true).trim().to_string()
}

/// `source`'s last line preprocesses to exactly `expect`.
fn is(source: &str, expect: &str) {
    let got = pp(source);
    let last = got.lines().last().unwrap_or("");
    assert_eq!(last, expect, "{source:?} preprocessed to {got:?}");
}

/// One round of macro expansion makes the boundary visible: `ID(9999b)`
/// prints `9999 b` from a tokenizer that split it.
#[test]
fn a_pp_number_is_one_token_whatever_literal_it_fails_to_be() {
    // pp-number identifier-nondigit
    is("#define ID(x) x\nID(9999b)", "9999b");
    is("#define ID(x) x\nID(123defg)", "123defg");
    // pp-number e sign: one token even though 0xE+12 can never convert
    is("#define ID(x) x\nID(0xE+12)", "0xE+12");
    is("#define ID(x) x\nID(1e+5)", "1e+5");
    // . digit opens one
    is("#define ID(x) x\nID(.5f)", ".5f");
    // and the shapes the old tokenizer already knew stay whole
    is("#define ID(x) x\nID(0x1p-3)", "0x1p-3");
    is("#define ID(x) x\nID(0xFFUL)", "0xFFUL");
    is("#define ID(x) x\nID(1.0f)", "1.0f");
}

/// Pasting two halves yields one pp-number, which must round-trip glued —
/// `12 ab` re-tokenizes as two tokens and is a different program.
#[test]
fn a_paste_that_forms_a_pp_number_stays_one_token() {
    is("#define CAT(a,b) a##b\nCAT(12,ab)", "12ab");
    is("#define CAT(a,b) a##b\nCAT(1e,5)", "1e5");
}

/// Stringizing keeps the number's whole spelling.
#[test]
fn a_stringized_pp_number_is_its_spelling() {
    is("#define S(x) #x\nS(123defg)", "\"123defg\"");
}

/// The other consumer of these tokens: `#if` arithmetic still reads every
/// integer-constant shape correctly after the wider munch.
#[test]
fn an_if_still_evaluates_every_integer_constant_shape() {
    let live = |cond: &str| {
        let text = pp(&format!("#if {cond}\nlive\n#else\ndead\n#endif"));
        assert_eq!(text, "live", "#if {cond} did not take the live branch");
    };
    live("0x10 == 16");
    live("010 == 8");
    live("1u < 2");
    live("255ul == 0xffL");
    live("(1 << 62) > 0");
}
