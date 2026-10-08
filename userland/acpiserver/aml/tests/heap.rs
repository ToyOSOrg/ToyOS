//! The interpreter's bound is heap bytes: under an allocator that counts
//! what this thread holds, an interpreter filled until it refuses holds at
//! most [`MAX_LIVE`], whatever a table fills it with, a load refused leaves
//! it holding what it held before, a nest of operators or of fields holds
//! one level's bytes past what the meter counts, never a level's each, and
//! the value an evaluation hands its caller is held to the bound while it
//! is built.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::*;
use toyos_aml::{Error, Interpreter, Value, MAX_LIVE};

struct Counting;

thread_local! {
    static HELD: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn moved(by: isize) {
    // A thread past its locals' end still frees: nothing measures it then.
    let _ = HELD.try_with(|h| {
        h.set(h.get() + by);
        let _ = PEAK.try_with(|p| p.set(p.get().max(h.get())));
    });
}

// SAFETY: every request is the system allocator's, unchanged; this only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        moved(l.size() as isize);
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        moved(-(l.size() as isize));
        unsafe { System.dealloc(p, l) }
    }

    // Counted as the realloc that moves: the new block is made before the
    // old one is freed, and both are held between.
    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        moved(size as isize);
        moved(-(l.size() as isize));
        unsafe { System.realloc(p, l, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// What this thread holds; the most it held is counted from here.
fn heap() -> isize {
    PEAK.set(HELD.get());
    HELD.get()
}

const BOUND: isize = MAX_LIVE as isize;

/// The refusal that is this bound's, and not the step bound's.
const FULL: &str = "more held live than one interpreter holds";

/// The NameSeg numbered `n`, of 1,213,056.
fn seg(n: usize) -> String {
    let digits = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    String::from_utf8(vec![b'A' + (n / 46656) as u8, digits[n / 1296 % 36], digits[n / 36 % 36], digits[n % 36]]).unwrap()
}

/// An interpreter holding one DSDT of `body`, and what the thread held before it.
fn start(body: &[u8]) -> (Interpreter, isize) {
    let t = dsdt(body);
    let before = heap();
    let mut i = Interpreter::new();
    i.load_bytes(&mut Sink, &t).expect("the DSDT loads");
    (i, before)
}

/// What an interpreter holds once it is full, and the most it held on the way.
struct Filled {
    held: isize,
    peak: isize,
    refused: Option<Error>,
}

impl Filled {
    fn of(before: isize, refused: Option<Error>) -> Filled {
        Filled { held: HELD.get() - before, peak: PEAK.get() - before, refused }
    }

    /// The harm first: the heap held, then that the bound is what ended the fill.
    fn within_the_bound(&self) {
        assert!(self.held <= BOUND, "{} bytes held, {} at most on the way", self.held, self.peak);
        assert_eq!(self.refused, Some(Error::Bound(FULL)), "nothing filled the interpreter");
    }
}

/// Loads `tables` until one is refused.
fn fill_by_loads(body: &[u8], tables: &[Vec<u8>]) -> Filled {
    let (mut i, before) = start(body);
    let refused = tables.iter().find_map(|t| i.load_bytes(&mut Sink, t).err());
    Filled::of(before, refused)
}

/// Evaluates `methods` until one is refused.
fn fill_by_methods(body: &[u8], methods: &[String]) -> Filled {
    let (mut i, before) = start(body);
    let refused = methods.iter().find_map(|m| i.evaluate(&mut Sink, m, &[]).err());
    Filled::of(before, refused)
}

/// A method storing `what` into every element of a package of 16,384 it
/// names, in a fifth of the steps one evaluation may take.
fn filler(n: usize, what: &[u8]) -> Vec<u8> {
    let pkg = format!("P{n:03}");
    cat(&[
        &def_name(&pkg, &var_package(&int(0x4000), &[])),
        &method(
            &format!("F{n:03}"),
            0,
            &cat(&[
                &store(&int(0), &local(0)),
                &while_(
                    &lless(&local(0), &int(0x4000)),
                    &cat(&[&store(what, &index(&name(&pkg), &local(0), &[0x00])), &increment(&local(0))]),
                ),
            ]),
        ),
    ])
}

fn fillers(count: usize, what: &[u8]) -> (Vec<u8>, Vec<String>) {
    let body: Vec<Vec<u8>> = (0..count).map(|n| filler(n, what)).collect();
    (body.concat(), (0..count).map(|n| format!("\\F{n:03}")).collect())
}

/// 204,000 field units in 51 devices, five bytes of table each.
fn dense() -> Vec<u8> {
    let units: Vec<Vec<u8>> = (0..4000).map(|u| unit(&seg(u)[1..], 8)).collect();
    let devices: Vec<Vec<u8>> = (0..51).map(|d| device(&format!("D{d:02}"), &field("\\MEM", 0x01, &units))).collect();
    table(b"SSDT", 2, &devices.concat())
}

fn region() -> Vec<u8> {
    op_region("MEM", 0x00, &int(0), &int(0x1000))
}

#[test]
fn buffers_of_a_mebibyte_fill_it_to_the_bound() {
    let (body, methods) = fillers(1, &buffer(&int(0x10_0000), &[]));
    let full = fill_by_methods(&body, &methods);
    full.within_the_bound();
    // The fill is the bound's: one buffer more would not fit, nor its copy.
    assert!(full.held > BOUND - (2 << 20), "{} bytes held", full.held);
}

/// A buffer of no bytes is still a shared record on the heap.
#[test]
fn empty_buffers_fill_it_to_the_bound() {
    let (body, methods) = fillers(16, &buffer(&int(0), &[]));
    fill_by_methods(&body, &methods).within_the_bound();
}

/// Each element a reference to a byte of one buffer: a record of its own.
#[test]
fn references_into_a_buffer_fill_it_to_the_bound() {
    let (body, methods) = fillers(16, &index(&name("BUF"), &int(0), &[0x00]));
    fill_by_methods(&cat(&[&def_name("BUF", &buffer(&int(1), &[])), &body]), &methods).within_the_bound();
}

/// Each element a name of one segment no table defines: 5,100 a table.
#[test]
fn package_elements_naming_nothing_yet_fill_it_to_the_bound() {
    let tables: Vec<Vec<u8>> = (0..64)
        .map(|t| {
            let packages: Vec<Vec<u8>> = (0..20).map(|p| def_name(&seg(p), &package(&vec![name("ZZZZ"); 255]))).collect();
            table(b"SSDT", 2, &device(&seg(t), &packages.concat()))
        })
        .collect();
    fill_by_loads(&[], &tables).within_the_bound();
}

/// 12,000 field units a table, which holds no method and so is not kept.
#[test]
fn field_units_fill_it_to_the_bound() {
    let units: Vec<Vec<u8>> = (0..4000).map(|u| unit(&seg(u)[1..], 8)).collect();
    let tables: Vec<Vec<u8>> = (0..40)
        .map(|t| {
            let devices: Vec<Vec<u8>> = (0..3).map(|d| device(&seg(t * 3 + d), &field("\\MEM", 0x01, &units))).collect();
            table(b"SSDT", 2, &devices.concat())
        })
        .collect();
    fill_by_loads(&region(), &tables).within_the_bound();
}

/// A parent of one child allocates a whole node of its map for it: 20,000
/// names a table, half of them an only child.
#[test]
fn devices_of_one_child_each_fill_it_to_the_bound() {
    let tables: Vec<Vec<u8>> = (0..40)
        .map(|t| {
            let devices: Vec<Vec<u8>> = (0..10_000).map(|d| device(&seg(t * 10_000 + d), &def_name("A", &int(0)))).collect();
            table(b"SSDT", 2, &devices.concat())
        })
        .collect();
    fill_by_loads(&[], &tables).within_the_bound();
}

/// A device nothing is named below holds no map, and none again once a
/// method has named an object below it and exited: 40,000 such devices, two
/// evaluations that each name an object below half of them, then empty
/// buffers to the bound.
#[test]
fn a_parent_whose_last_child_is_unwound_holds_no_map() {
    let devices: Vec<Vec<u8>> = (0..40_000).map(|d| device(&seg(d), &[])).collect();
    let devices = table(b"SSDT", 2, &devices.concat());
    let (fill, fills) = fillers(8, &buffer(&int(0), &[]));
    let below = |half: usize| {
        let names: Vec<Vec<u8>> = (0..20_000).map(|d| def_name(&format!("\\{}.T", seg(half * 20_000 + d)), &int(0))).collect();
        method(&format!("M{half}"), 0, &names.concat())
    };
    let methods = table(b"SSDT", 2, &cat(&[&below(0), &below(1)]));
    let (mut i, before) = start(&fill);
    for t in [&devices, &methods] {
        i.load_bytes(&mut Sink, t).expect("the table loads");
    }
    let held = heap() - before;
    for m in ["\\M0", "\\M1"] {
        assert_eq!(i.evaluate(&mut Sink, m, &[]), Ok(Value::Uninitialized));
    }
    let kept = HELD.get() - before;
    let maps = PEAK.get() - before - held;
    let refused = fills.iter().find_map(|m| i.evaluate(&mut Sink, m, &[]).err());
    Filled::of(before, refused).within_the_bound();
    // The names were made: 20,000 parents each held a map while one ran.
    assert!(maps >= 1 << 20, "{maps} bytes held by an evaluation while it ran");
    assert_eq!(kept, held, "bytes held after the two evaluations, and before them");
}

/// One table naming 204,000 field units is refused, and the interpreter
/// then holds what it held before, to the byte: its arena has the slots it
/// had, and what it had room for it has room for again.
#[test]
fn a_refused_load_gives_back_what_it_took() {
    let refused = dense();
    // Fourteen buffers of a mebibyte, and the fifteenth and sixteenth the
    // last store makes and copies: all an interpreter holds.
    let fifteen = cat(&[
        &store(&int(0), &local(0)),
        &while_(
            &lless(&local(0), &int(14)),
            &cat(&[&store(&buffer(&int(0x10_0000), &[]), &index(&name("PKG"), &local(0), &[0x00])), &increment(&local(0))]),
        ),
    ]);
    let (mut i, before) = start(&cat(&[&region(), &def_name("PKG", &package(&vec![int(0); 14])), &method("FILL", 0, &fifteen)]));
    let held = HELD.get() - before;
    let r = i.load_bytes(&mut Sink, &refused);
    let after = Filled::of(before, r.err());
    assert_eq!(after.held, held, "bytes held after the refusal, and before the load");
    assert!(after.peak <= BOUND, "{} bytes held on the way", after.peak);
    assert_eq!(after.refused, Some(Error::Bound(FULL)));
    assert!(matches!(i.evaluate(&mut Sink, "\\D00.AAA", &[]), Err(Error::NotFound(_))));
    assert_eq!(i.evaluate(&mut Sink, "\\FILL", &[]), Ok(Value::Uninitialized));
}

/// Fifteen packages of 65,000 integers, each the last element of the one
/// around it, in one table of under a mebibyte: each is held before it is
/// filled, so the load is refused at the bound and never holds more.
#[test]
fn a_nest_of_packages_is_held_while_it_is_read() {
    let mut nest: Option<Vec<u8>> = None;
    for _ in 0..15 {
        let mut elems = vec![vec![0x00]; 65_000];
        elems.extend(nest.take());
        nest = Some(var_package(&int(elems.len() as u64), &elems));
    }
    let t = table(b"SSDT", 2, &def_name("NEST", &nest.unwrap()));
    let (mut i, before) = start(&[]);
    let r = i.load_bytes(&mut Sink, &t);
    let read = Filled::of(before, r.err());
    assert!(read.peak <= BOUND, "{} bytes held on the way", read.peak);
    assert_eq!(read.refused, Some(Error::Bound(FULL)));
}

/// The most a nest below holds past what its interpreter held before it,
/// or past the bound: one level's bytes, copied and grown once, where a
/// level's each is eight mebibytes at the least.
const LEVEL: isize = 4 << 20;

/// The most evaluating `path` held past what the interpreter of `body` held
/// before it, and what it answered.
fn most(body: &[u8], path: &str) -> (isize, Result<Value, Error>) {
    let (mut i, _) = start(body);
    let held = heap();
    let r = i.evaluate(&mut Sink, path, &[]);
    (PEAK.get() - held, r)
}

fn mebibyte() -> Vec<u8> {
    def_name("BUF", &buffer(&int(0x10_0000), &[]))
}

/// ToString copies its source after its length is evaluated: 32 of them,
/// each the length of the one around it, over one buffer of a mebibyte.
#[test]
fn a_nest_of_to_strings_holds_one_copy() {
    let length = (0..32).fold(int(1), |inner, _| lequal(&cat(&[&[0x9C], &name("BUF"), &inner, &[0x00]]), &string("")));
    let (held, r) = most(&cat(&[&mebibyte(), &method("NEST", 0, &length)]), "\\NEST");
    assert_eq!(r, Ok(Value::Uninitialized));
    // A copy was made: the nest is over the buffer, and not over nothing.
    assert!(held >= 1 << 20, "{held} bytes held past what was held before");
    assert!(held <= LEVEL, "{held} bytes held past what was held before");
}

/// Mid lets its source's copy go before its target is evaluated: 33 of
/// them, each the buffer the one around it indexes for its target, over one
/// buffer of a mebibyte.
#[test]
fn a_nest_of_mids_holds_one_copy() {
    let mid = |target: &[u8]| cat(&[&[0x9E], &name("BUF"), &int(0), &int(1), target]);
    let nest = (0..32).fold(mid(&[0x00]), |inner, _| mid(&index(&inner, &int(0), &[0x00])));
    let (held, r) = most(&cat(&[&mebibyte(), &method("NEST", 0, &nest)]), "\\NEST");
    assert_eq!(r, Ok(Value::Uninitialized));
    assert!(held >= 1 << 20, "{held} bytes held past what was held before");
    assert!(held <= LEVEL, "{held} bytes held past what was held before");
}

/// Concatenate copies what leads after it has named the type of what
/// follows. Naming a package element resolves it, here to a PCI_Config
/// field, whose access asks the device's `_ADR`, which is the method that
/// concatenates: 16 deep, over `LEAD` of half a mebibyte. `CNT` counts the
/// invocations that concatenate, each inside the one before it: the most
/// held is of a nest only if it reads 16.
fn joined_through_a_field(lead: &[u8]) -> isize {
    let join = op2(0x73, &name("LEAD"), &index(&name("PKG"), &int(0), &[0x00]), &[0x00]);
    let deeper = if_(&lless(&name("CNT"), &int(16)), &cat(&[&increment(&name("CNT")), &join]));
    let bridge = cat(&[
        &def_name("_BBN", &int(0)),
        &def_name("CNT", &int(0)),
        lead,
        &def_name("PKG", &package(&[name("FLD")])),
        &method("_ADR", 0, &cat(&[&deeper, &ret(&int(0))])),
        &op_region("CFG", 0x02, &int(0), &int(0x10)),
        &field("CFG", 0x01, &[unit("FLD", 8)]),
    ]);
    let (mut i, _) = start(&device("PCI0", &bridge));
    let held = heap();
    let r = i.evaluate(&mut Sink, "\\PCI0._ADR", &[]);
    let most = PEAK.get() - held;
    assert_eq!(r, Ok(Value::Integer(0)));
    assert_eq!(i.evaluate(&mut Sink, "\\PCI0.CNT", &[]), Ok(Value::Integer(16)), "the depth the nest reached");
    assert!(most >= 1 << 19, "{most} bytes held past what was held before");
    most
}

#[test]
fn a_nest_of_concatenates_holds_one_buffers_copy() {
    let held = joined_through_a_field(&def_name("LEAD", &buffer(&int(0x8_0000), &[])));
    assert!(held <= LEVEL, "{held} bytes held past what was held before");
}

/// The string that leads is 524,287 characters, an operator's making, which
/// Name does not take: made at load.
#[test]
fn a_nest_of_concatenates_holds_one_strings_copy() {
    let decimal = op1(0x97, &buffer(&int(0x4_0000), &[]), &[0x00]);
    let held = joined_through_a_field(&cat(&[&def_name("LEAD", &string("")), &store(&decimal, &name("LEAD"))]));
    assert!(held <= LEVEL, "{held} bytes held past what was held before");
}

/// 33 fields of a mebibyte, each read through the one before it: every one
/// holds its bytes against the bound before its first unit is read, so the
/// read is refused at the bound and never holds a level's each.
#[test]
fn fields_read_through_each_other_are_held_while_they_are_read() {
    let through: Vec<Vec<u8>> = (0..32)
        .map(|d| index_field("IDX", &format!("D{d:03}"), 0x01, &[unit(&format!("D{:03}", d + 1), 0x80_0000)]))
        .collect();
    let body = cat(&[
        &op_region("MEM", 0x00, &int(0), &int(0x20_0000)),
        &field("MEM", 0x01, &[unit("IDX", 8), unit("D000", 0x80_0000)]),
        &through.concat(),
    ]);
    let (mut i, before) = start(&body);
    let r = i.evaluate(&mut Sink, "\\D032", &[]);
    let read = Filled::of(before, r.err());
    assert!(read.peak <= BOUND + LEVEL, "{} bytes held on the way", read.peak);
    assert_eq!(read.refused, Some(Error::Bound(FULL)));
}

/// A package element that names an object is copied for the caller each
/// time it is read: 63 naming one buffer of a mebibyte are 63 MiB of value
/// within the step bound. The value is held against the bound while it is
/// built, so the evaluation is refused there, and only buffers held one
/// beside another reach it.
#[test]
fn a_returned_value_of_buffers_is_held_while_it_is_built() {
    let body = cat(&[&def_name("PKG", &package(&vec![name("BUF"); 63])), &mebibyte()]);
    let (mut i, before) = start(&body);
    let r = i.evaluate(&mut Sink, "\\PKG", &[]);
    let built = Filled::of(before, r.err());
    assert!(built.peak <= BOUND, "{} bytes held on the way", built.peak);
    assert_eq!(built.refused, Some(Error::Bound(FULL)));
}

/// 40 package elements naming one package of 65,000, defined after them.
fn packages() -> Vec<u8> {
    cat(&[&def_name("PKG", &package(&vec![name("WIDE"); 40])), &def_name("WIDE", &var_package(&int(65_000), &[]))])
}

/// The same of 40 elements naming one package of 65,000: its elements cost
/// the caller more than they cost the interpreter.
#[test]
fn a_returned_value_of_packages_is_held_while_it_is_built() {
    let (mut i, before) = start(&packages());
    let r = i.evaluate(&mut Sink, "\\PKG", &[]);
    let built = Filled::of(before, r.err());
    assert!(built.peak <= BOUND, "{} bytes held on the way", built.peak);
    assert_eq!(built.refused, Some(Error::Bound(FULL)));
}

/// What an evaluation held for its caller's value it gives back, once and
/// whole, when the value is the caller's: the package of 65,000 answers
/// sixteen times over on one interpreter, each value dropped, where what
/// sixteen answers hold together is twice the bound and more.
#[test]
fn a_returned_value_is_given_back_when_it_is_the_callers() {
    let (mut i, _) = start(&packages());
    for n in 0..16 {
        let v = i.evaluate(&mut Sink, "\\WIDE", &[]);
        assert_eq!(v.map(|v| matches!(v, Value::Package(p) if p.len() == 65_000)), Ok(true), "evaluation {n}");
    }
}

/// Filled with references until 48 bytes more do not fit: a field that fits
/// an Integer is still read, gathered without the heap, where a wider one's
/// buffer is refused.
#[test]
fn a_full_interpreter_reads_a_field_that_fits_an_integer() {
    let (fill, fills) = fillers(16, &index(&name("BUF"), &int(0), &[0x00]));
    let fields = cat(&[&region(), &field("MEM", 0x01, &[unit("NARW", 64), unit("WIDE", 72)])]);
    let (mut i, _) = start(&cat(&[&def_name("BUF", &buffer(&int(1), &[])), &fields, &fill]));
    let refused = fills.iter().find_map(|m| i.evaluate(&mut Sink, m, &[]).err());
    assert_eq!(refused, Some(Error::Bound(FULL)));
    assert_eq!(i.evaluate(&mut Sink, "\\NARW", &[]), Ok(Value::Integer(0)));
    assert_eq!(i.evaluate(&mut Sink, "\\WIDE", &[]), Err(Error::Bound(FULL)));
}
