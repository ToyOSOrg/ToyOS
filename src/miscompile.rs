//! What a sysroot's compiler has been seen to miscompile, compiled by every
//! sysroot before it is published and refused where the answer is wrong.
//!
//! **A loop keeps the exit its counter's maximum takes.** An LLVM whose
//! ScalarEvolution gives a header phi's recurrence the wrap flags of its
//! increment without proving them for it (llvm/llvm-project#175729) lets
//! `indvars` fold such a loop's last exit to `false`: safe Rust becomes an
//! endless loop. Held twice, by the two compilers a toolchain carries of one
//! LLVM: [`LAST_EXIT_IR`] is the loop itself, which no front end can give
//! another shape, and [`LAST_EXIT`] the Rust that was seen to make it.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::arch::Arch;
use crate::clang::CSysroot;
use crate::toolchain::GUEST_TARGETS;

/// The loop as LLVM IR, whose `caller` returns `true`.
const LAST_EXIT_IR: &str = include_str!("miscompile/last_exit.ll");

/// An inclusive range walked to its integer type's maximum, whose `caller`
/// returns `true`.
const LAST_EXIT: &str = include_str!("miscompile/last_exit.rs");

/// Refuse the toolchain at `toolchain` unless its clang compiles
/// [`LAST_EXIT_IR`] to `true` for each architecture, and its rustc
/// [`LAST_EXIT`] for every guest target, at the optimisation level guests are
/// built with. Sources and IR are written in `scratch`.
pub(crate) fn refuse(toolchain: &Path, scratch: &Path) {
    eprintln!("Checking that the compilers of {} keep a loop's last exit", toolchain.display());
    fs::create_dir_all(scratch).unwrap_or_else(|e| panic!("create {}: {e}", scratch.display()));
    let loop_ir = scratch.join("last_exit.ll");
    fs::write(&loop_ir, LAST_EXIT_IR).unwrap_or_else(|e| panic!("write {}: {e}", loop_ir.display()));
    for arch in Arch::ALL {
        let c = CSysroot::of(toolchain, arch);
        let ir = scratch.join(format!("last_exit-clang-{}.ll", c.target));
        let mut clang = Command::new(&c.clang);
        clang.arg(format!("--target={}", c.target)).args(["-O2", "-S", "-emit-llvm", "-o"]).arg(&ir).arg(&loop_ir);
        keeps_the_exit(&mut clang, &ir, c.target);
    }
    let source = scratch.join("last_exit.rs");
    fs::write(&source, LAST_EXIT).unwrap_or_else(|e| panic!("write {}: {e}", source.display()));
    for target in GUEST_TARGETS.map(|target| target.triple()) {
        let ir = scratch.join(format!("last_exit-rustc-{target}.ll"));
        let mut rustc = Command::new(toolchain.join("bin/rustc"));
        rustc
            .args(["--edition", "2021", "--crate-type", "lib", "--target", target])
            .args(["-C", "opt-level=2", "-C", "codegen-units=1", "--emit", "llvm-ir", "-o"])
            .arg(&ir)
            .arg(&source);
        keeps_the_exit(&mut rustc, &ir, target);
    }
}

/// Run `compiler`, which writes `ir` for `target`, and refuse it unless
/// `caller` there is one block that returns `true`.
fn keeps_the_exit(compiler: &mut Command, ir: &Path, target: &str) {
    let output = compiler.output().unwrap_or_else(|e| panic!("run {compiler:?}: {e}"));
    assert!(output.status.success(), "{compiler:?} failed:\n{}", String::from_utf8_lossy(&output.stderr));
    let text = fs::read_to_string(ir).unwrap_or_else(|e| panic!("read {}: {e}", ir.display()));
    assert!(
        returns_true(&text, "caller"),
        "{compiler:?} miscompiles a loop that ends at its counter's maximum for {target}: `caller` returns \
         `true` and its IR does not (llvm/llvm-project#175729):\n{}",
        body(&text, "caller").unwrap_or("there is no `caller`"),
    );
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

    /// **The negative control is the defect itself**: `caller` verbatim as a
    /// compiler with the fault emitted it for `aarch64-unknown-toyos`, and as
    /// one without it does.
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
