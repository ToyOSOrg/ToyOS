//! toyos-ld is frozen with a TLS layout the loader no longer shares: it
//! resolves an executable's local-exec accesses against the block's unrounded
//! size, and the loader ends that block at its size rounded to `p_align`. An
//! executable with thread-local storage is refused rather than linked wrong; a
//! shared object's accesses go through the loader and are linked as before.

mod common;

use common::{ObjBuilder, RET};
use object::write::{Symbol, SymbolSection};
use object::{SymbolFlags, SymbolKind, SymbolScope};

fn with_tls(entry: bool) -> Vec<u8> {
    let mut b = ObjBuilder::new();
    if entry {
        b.text("_start", &[RET], SymbolScope::Dynamic);
    }
    let obj = b.object();
    let tdata = obj.section_id(object::write::StandardSection::Tls);
    // Five bytes, so the block's size is not its 64-byte alignment's multiple.
    let offset = obj.append_section_data(tdata, &[1, 2, 3, 4, 5], 64);
    obj.add_symbol(Symbol {
        name: b"counter".to_vec(),
        value: offset,
        size: 5,
        kind: SymbolKind::Tls,
        scope: SymbolScope::Dynamic,
        weak: false,
        section: SymbolSection::Section(tdata),
        flags: SymbolFlags::None,
    });
    b.finish()
}

#[test]
fn an_executable_with_thread_local_storage_is_refused() {
    let refused = toyos_ld::link_full(&[("a.o".to_string(), with_tls(true))], "_start", false, false);
    assert!(
        matches!(refused, Err(toyos_ld::LinkError::TlsExecutable)),
        "linked: {:?}",
        refused.map(|out| out.len())
    );
}

#[test]
fn an_executable_without_it_still_links() {
    let mut b = ObjBuilder::new();
    b.text("_start", &[RET], SymbolScope::Dynamic);
    toyos_ld::link_full(&[("a.o".to_string(), b.finish())], "_start", false, false).expect("no TLS, linked");
}

#[test]
fn a_shared_object_with_thread_local_storage_still_links() {
    toyos_ld::link_shared(&[("a.o".to_string(), with_tls(false))]).expect("a library's TLS, linked");
}
