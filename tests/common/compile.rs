use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::fs;

use toyos_build::clang::CSysroot;

/// Root of the repository.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Directory containing the TinyCC test cases.
pub fn testcases_dir() -> PathBuf {
    repo_root().join("tests/testcases/tinycc")
}

/// The C sysroot every C program here is built against, and the clang that
/// builds it: the toolchain's own, for the suite's architecture. Once per
/// process — a hundred and fifty C programs build against it.
pub fn c_sysroot() -> CSysroot {
    static C: OnceLock<CSysroot> = OnceLock::new();
    C.get_or_init(|| {
        let mut lock = toyos_build::buildlock::shared(&repo_root(), "the C sysroot");
        let sysroot = toyos_build::toolchain::ensure(&repo_root(), false, &mut lock);
        CSysroot::of(&sysroot.dir, super::qemu::SUITE_ARCH)
    })
    .clone()
}

/// The one flag the corpus is compiled with beyond the target and sysroot:
/// TinyCC takes a declaration with no type as an implicit `int` and warns, and
/// clang refuses one from C99 on. `102_alignas` is written with one.
const CORPUS_FLAGS: &[&str] = &["-Wno-error=implicit-int"];

/// Where `name`'s object or binary is written, in this lane.
fn scratch(name: &str, what: &str) -> PathBuf {
    super::lane::dir().join(format!("{name}{what}"))
}

/// The first error a failed `clang` reported, which is what a declared case in
/// `NOT_RUN` quotes.
fn first_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .find(|l| l.contains("error:"))
        .or_else(|| text.lines().next())
        .unwrap_or("clang failed and said nothing")
        .to_string()
}

/// Compile one of the corpus's files to an object with the toolchain's clang,
/// from the corpus's own directory and by its bare name, as TinyCC's runner
/// does: `__FILE__` is part of what some cases print.
fn compile_one(c: &CSysroot, source: &str, object: &Path) {
    let output = Command::new(&c.clang)
        .args(c.args())
        .args(CORPUS_FLAGS)
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(object)
        .current_dir(testcases_dir())
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", c.clang.display()));
    assert!(output.status.success(), "{}", first_error(&output.stderr));
}

/// Compile a corpus case, and its companion if it has one (`104+_inline.c`
/// for `104_inline`), and return the objects.
pub fn compile_c(name: &str) -> Vec<PathBuf> {
    let c = c_sysroot();
    let mut sources = vec![format!("{name}.c")];
    if let Some((prefix, suffix)) = name.split_once('_') {
        let companion = format!("{prefix}+_{suffix}.c");
        if testcases_dir().join(&companion).exists() {
            sources.push(companion);
        }
    }
    sources
        .iter()
        .enumerate()
        .map(|(i, source)| {
            let object = scratch(name, &format!("-{i}.o"));
            compile_one(&c, source, &object);
            object
        })
        .collect()
}

/// Link flags a case needs beyond the corpus's, by case name.
///
/// An image whose lowest address is 0 hides a table reported from the load bias
/// instead of the image's first byte; the case prints its lowest `PT_LOAD`, so
/// an entry that stops reaching its case reds it.
const LINK_FLAGS: &[(&str, &[&str])] = &[("205_dl_iterate_phdr_image_base", &["-Wl,--image-base=0x200000"])];

/// Link `objects` into a ToyOS executable through the clang driver — which
/// names `ld.lld` and the sysroot's `libtoyos_c.a`, and makes a PIE — and
/// return its bytes.
pub fn link_toyos(objects: &[PathBuf], name: &str) -> Vec<u8> {
    let c = c_sysroot();
    let out = scratch(name, ".elf");
    let output = Command::new(&c.clang)
        .args(c.args())
        .args(objects)
        .args(LINK_FLAGS.iter().filter(|(case, _)| *case == name).flat_map(|(_, flags)| flags.iter()))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", c.clang.display()));
    for object in objects {
        let _ = fs::remove_file(object);
    }
    // One line: the harness reads a failed link for the symbol a declared case
    // stops on, and LLD reports each undefined symbol on a line of its own.
    assert!(
        output.status.success(),
        "clang could not link {name}: {}",
        String::from_utf8_lossy(&output.stderr).lines().collect::<Vec<_>>().join(" | "),
    );
    let linked = fs::read(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()));
    let _ = fs::remove_file(&out);
    linked
}
