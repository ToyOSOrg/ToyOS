//! What a sysroot's compiler has been seen to miscompile, compiled by every
//! sysroot before it is published and refused where the answer is wrong.
//!
//! **A loop keeps the exit its counter's maximum takes** ([`LAST_EXIT`]). An
//! LLVM whose ScalarEvolution gives a header phi's recurrence the wrap flags of
//! its increment without proving them for it (llvm/llvm-project#175729, the
//! fork's `-scev-unconditional-preinc-nowrap-flags`) lets `indvars` fold such a
//! loop's last exit to `false`: safe Rust becomes an endless loop.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::arch::Arch;

/// An inclusive range walked to its integer type's maximum, whose `caller`
/// returns `true`.
const LAST_EXIT: &str = include_str!("miscompile/last_exit.rs");

/// The line of [`LAST_EXIT`] that names the range's integer type.
const PORT: &str = "type Port = u16;";

/// The types [`LAST_EXIT`] is compiled over: the two an unfixed compiler made
/// an endless loop of, `u16` for AArch64 and `u128` for both architectures.
const PORTS: [&str; 2] = ["u16", "u128"];

/// Refuse the toolchain at `toolchain` unless its rustc compiles [`LAST_EXIT`]
/// to `true` for each of [`PORTS`] and each architecture's userland, at the
/// optimisation level guests are built with. Sources and IR are written in
/// `scratch`.
pub(crate) fn refuse(toolchain: &Path, scratch: &Path) {
    fs::create_dir_all(scratch).unwrap_or_else(|e| panic!("create {}: {e}", scratch.display()));
    assert!(LAST_EXIT.contains(PORT), "the reproducer names its integer type as `{PORT}`, and no longer does");
    for port in PORTS {
        let source = scratch.join(format!("last_exit_{port}.rs"));
        fs::write(&source, LAST_EXIT.replace(PORT, &format!("type Port = {port};")))
            .unwrap_or_else(|e| panic!("write {}: {e}", source.display()));
        for arch in Arch::ALL {
            let target = arch.userland();
            let ir = scratch.join(format!("last_exit_{port}-{target}.ll"));
            let rustc = toolchain.join("bin/rustc");
            let output = Command::new(&rustc)
                .args(["--edition", "2021", "--crate-type", "lib", "--target", target])
                .args(["-C", "opt-level=2", "-C", "codegen-units=1", "--emit", "llvm-ir", "-o"])
                .arg(&ir)
                .arg(&source)
                .env_remove("RUSTFLAGS")
                .output()
                .unwrap_or_else(|e| panic!("run {}: {e}", rustc.display()));
            assert!(
                output.status.success(),
                "{} did not compile {} for {target}:\n{}",
                rustc.display(),
                source.display(),
                String::from_utf8_lossy(&output.stderr),
            );
            let text = fs::read_to_string(&ir).unwrap_or_else(|e| panic!("read {}: {e}", ir.display()));
            assert!(
                returns_true(&text, "caller"),
                "{} miscompiles a loop over `{port}` that ends at `{port}::MAX` for {target}: `caller` \
                 returns `true` and its IR does not (llvm/llvm-project#175729):\n{}",
                rustc.display(),
                body(&text, "caller").unwrap_or("there is no `caller`"),
            );
        }
    }
}

/// The definition of `function` in the LLVM IR `ir`, from its `define` to its
/// closing brace.
fn body<'a>(ir: &'a str, function: &str) -> Option<&'a str> {
    let named = format!("@{function}(");
    let mut at = 0;
    let start = ir.split_inclusive('\n').find_map(|line| {
        let here = at;
        at += line.len();
        (line.starts_with("define ") && line.contains(&named)).then_some(here)
    })?;
    let end = ir[start..].find("\n}")?;
    Some(&ir[start..start + end + 2])
}

/// Whether `function` in `ir` is one block that returns `true`.
fn returns_true(ir: &str, function: &str) -> bool {
    let Some(body) = body(ir, function) else { return false };
    let instructions: Vec<&str> =
        body.lines().skip(1).map(str::trim).filter(|line| !line.is_empty() && !line.ends_with(':') && *line != "}").collect();
    instructions == ["ret i1 true"]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The negative control is the defect itself**: `caller` verbatim as the
    /// unfixed compiler emitted it for `aarch64-unknown-toyos` over `u16`, and
    /// as the same source compiles for `x86_64-unknown-toyos`.
    #[test]
    fn an_endless_caller_is_not_one_that_returns_true() {
        let endless = "\
; Function Attrs: nofree norecurse noreturn nosync nounwind memory(none)
define noundef zeroext i1 @caller() unnamed_addr #0 personality ptr @rust_eh_personality {
start:
  br label %bb4.i.backedge.i

bb4.i.backedge.i:                                 ; preds = %bb4.i.backedge.i, %start
  br label %bb4.i.backedge.i
}

attributes #0 = { nofree norecurse noreturn nosync nounwind memory(none) }
";
        assert!(!returns_true(endless, "caller"));
        let right = "\
define noundef zeroext i1 @caller() unnamed_addr #0 {
start:
  ret i1 true
}
";
        assert!(returns_true(right, "caller"));
        assert!(!returns_true(&right.replace("ret i1 true", "ret i1 false"), "caller"));
        assert!(!returns_true(&right.replace("@caller(", "@other("), "caller"), "a module with no `caller` answers nothing");
    }
}
