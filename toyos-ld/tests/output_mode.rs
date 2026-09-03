//! What the linker writes is a program, and a program nothing may exec is not
//! one. Every caller in this repository feeds the output to a loader that does
//! not read the mode, so a consumer linking by hand is who finds out.

mod common;

use common::{Case, ObjBuilder, RET};
use object::SymbolScope;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn a_linked_program_is_executable() {
    let mut b = ObjBuilder::new();
    b.text("_start", &[RET], SymbolScope::Dynamic);
    let case = Case::new("mode").input("a.o", b.finish()).arg("-static");

    let out = std::env::temp_dir().join(format!("toyos-ld-mode-{}", std::process::id()));
    let _ = std::fs::remove_file(&out);
    case.link_once(&out);

    let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "{} was written 0{mode:o}", out.display());
}
