//! The PPTT: QEMU `virt`'s own table placed against the command line that
//! built it, and every refusal over crafted ones.

mod common;

use common::{reseal, Machine};
use toyos_acpi::{Place, Pptt, PpttRefused, Table};

const BASE: u64 = 0x1000;

fn open(bytes: &[u8]) -> Result<Pptt<Machine<'static>>, PpttRefused> {
    // Leaked: a test's table lives as long as the test binary.
    let regions: &'static [(u64, &'static [u8])] =
        Box::leak(Box::new([(BASE, &*Box::leak(bytes.to_vec().into_boxed_slice()))]));
    let machine = Machine { regions };
    Pptt::open(Table::open(machine, BASE, b"PPTT", 36)?)
}

/// `-smp 8,sockets=1,clusters=2,cores=2,threads=2` with an L3 per cluster
/// (`fixtures/qemu-11.1.1-virt-pptt/SOURCE`): processors 2k and 2k+1 share a
/// core, 0-3 and 4-7 an LLC, and all eight a package.
#[test]
fn qemus_table_places_each_processor_as_its_command_line_asked() {
    let pptt = open(include_bytes!("../fixtures/qemu-11.1.1-virt-pptt/pptt.bin")).unwrap();
    let places: Vec<Place> = (0..8).map(|uid| pptt.place(uid).unwrap()).collect();
    for uid in 0..8 {
        for other in 0..8 {
            let (a, b) = (places[uid], places[other]);
            assert_eq!(a.core == b.core, uid / 2 == other / 2, "core of {uid} and {other}");
            assert_eq!(a.llc == b.llc, uid / 4 == other / 4, "llc of {uid} and {other}");
            assert_eq!(a.package, b.package, "package of {uid} and {other}");
        }
    }
    assert_eq!(pptt.place(8), Err(PpttRefused::NoLeaf { uid: 8 }));
}

/// A table under construction: structures appended after the header, each
/// answering its own offset so later ones can refer to it.
struct Builder(Vec<u8>);

const PACKAGE: u32 = 1 << 0;
const ID_VALID: u32 = 1 << 1;
const THREAD: u32 = 1 << 2;
const LEAF: u32 = 1 << 3;

impl Builder {
    fn new(revision: u8) -> Self {
        let mut header = vec![0u8; 36];
        header[..4].copy_from_slice(b"PPTT");
        header[8] = revision;
        Self(header)
    }

    fn node(&mut self, flags: u32, parent: u32, id: u32, resources: &[u32]) -> u32 {
        let at = self.0.len() as u32;
        self.0.extend_from_slice(&[0, 20 + 4 * resources.len() as u8, 0, 0]);
        for word in [flags, parent, id, resources.len() as u32].iter().chain(resources) {
            self.0.extend_from_slice(&word.to_le_bytes());
        }
        at
    }

    /// A cache whose attributes byte is `kind << 2`: 0 data, 1 instruction, 2
    /// unified, its type declared valid (flags bit 4).
    fn cache(&mut self, kind: u8) -> u32 {
        let at = self.untyped_cache(kind);
        self.0[at as usize + 4] = 1 << 4;
        at
    }

    /// The same with its type not declared valid.
    fn untyped_cache(&mut self, kind: u8) -> u32 {
        let at = self.0.len() as u32;
        let mut cache = vec![0u8; 24];
        cache[0] = 1;
        cache[1] = 24;
        cache[21] = kind << 2;
        self.0.extend_from_slice(&cache);
        at
    }

    fn set_parent(&mut self, node: u32, parent: u32) {
        let at = node as usize + 8;
        self.0[at..at + 4].copy_from_slice(&parent.to_le_bytes());
    }

    fn bytes(mut self) -> Vec<u8> {
        let len = self.0.len() as u32;
        self.0[4..8].copy_from_slice(&len.to_le_bytes());
        reseal(&mut self.0);
        self.0
    }
}

/// Each processor's own L1 and nothing shared: each core is its own LLC.
#[test]
fn private_caches_alone_make_each_core_its_own_llc() {
    let mut t = Builder::new(3);
    let package = t.node(PACKAGE, 0, 0, &[]);
    let l1 = t.cache(0);
    let a = t.node(LEAF | ID_VALID, package, 7, &[l1]);
    let b = t.node(LEAF | ID_VALID, package, 9, &[l1]);
    let pptt = open(&t.bytes()).unwrap();
    assert_eq!(pptt.place(7), Ok(Place { core: a, llc: a, package }));
    assert_eq!(pptt.place(9), Ok(Place { core: b, llc: b, package }));
}

/// An instruction cache is no LLC, so a table that gives a processor no
/// data or unified cache puts its LLC at the package.
#[test]
fn an_instruction_cache_alone_is_no_llc() {
    let mut t = Builder::new(2);
    let i = t.cache(1);
    let package = t.node(PACKAGE, 0, 0, &[i]);
    let core = t.node(0, package, 0, &[]);
    t.node(LEAF | ID_VALID | THREAD, core, 1, &[]);
    let pptt = open(&t.bytes()).unwrap();
    assert_eq!(pptt.place(1), Ok(Place { core, llc: package, package }));
}

/// A cache whose type is not declared valid still says a cache is there, so
/// it places its node whatever its type bits read.
#[test]
fn a_cache_of_undeclared_type_still_places_its_node() {
    let mut t = Builder::new(2);
    let l3 = t.untyped_cache(1);
    let package = t.node(PACKAGE, 0, 0, &[]);
    let cluster = t.node(0, package, 0, &[l3]);
    let leaf = t.node(LEAF | ID_VALID, cluster, 3, &[]);
    let pptt = open(&t.bytes()).unwrap();
    assert_eq!(pptt.place(3), Ok(Place { core: leaf, llc: cluster, package }));
}

#[test]
fn a_table_that_places_no_processor_is_refused_by_name() {
    assert_eq!(open(&Builder::new(1).bytes()).err(), Some(PpttRefused::Revision(1)));

    // A node whose resource count runs past its own length.
    let mut t = Builder::new(2);
    let at = t.node(PACKAGE, 0, 0, &[]);
    let mut bytes = t.bytes();
    bytes[at as usize + 16] = 1;
    common::reseal(&mut bytes);
    assert_eq!(open(&bytes).err(), Some(PpttRefused::Structure { at: at as usize, declared: 20 }));

    // A parent that is a cache.
    let mut t = Builder::new(2);
    let cache = t.cache(2);
    let leaf = t.node(LEAF | ID_VALID, cache, 0, &[]);
    assert_eq!(
        open(&t.bytes()).unwrap().place(0),
        Err(PpttRefused::Parent { at: leaf as usize, parent: cache })
    );

    // No package above the processor.
    let mut t = Builder::new(2);
    let root = t.node(0, 0, 0, &[]);
    t.node(LEAF | ID_VALID, root, 0, &[]);
    assert_eq!(open(&t.bytes()).unwrap().place(0), Err(PpttRefused::NoPackage { uid: 0 }));

    // Two nodes each the other's parent.
    let mut t = Builder::new(2);
    let a = t.node(0, 0, 0, &[]);
    let b = t.node(0, a, 0, &[]);
    t.set_parent(a, b);
    t.node(LEAF | ID_VALID, a, 0, &[]);
    assert_eq!(open(&t.bytes()).unwrap().place(0), Err(PpttRefused::Cycle { uid: 0 }));

    // A leaf without a valid processor ID names no processor.
    let mut t = Builder::new(2);
    let package = t.node(PACKAGE, 0, 0, &[]);
    t.node(LEAF, package, 0, &[]);
    assert_eq!(open(&t.bytes()).unwrap().place(0), Err(PpttRefused::NoLeaf { uid: 0 }));
}
