//! The interpreter's bound is heap bytes: under an allocator that counts
//! what this thread holds, an interpreter filled until it refuses holds at
//! most [`MAX_LIVE`], whatever a table fills it with, and a load refused
//! leaves it holding what it held before.

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

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        moved(size as isize - l.size() as isize);
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

/// One table naming 204,000 field units is refused, and the interpreter
/// then holds what it held before: its arena has shrunk, and what it had
/// room for it has room for again.
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
    // The namespace's arena keeps at most four slots for each node it holds.
    assert!(after.held <= 4 * held, "{} bytes held after the refusal, {held} before the load", after.held);
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
