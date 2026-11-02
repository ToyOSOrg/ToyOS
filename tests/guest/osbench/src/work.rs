//! A build-shaped workload, **a proxy and not a compiler**: a fixed set of
//! source-like text files, each turned into an output file by a child
//! process of its own, as many at once as there are CPUs, a new one started
//! each time the oldest running child ends. Each child (`osbench cc <in>
//! <out>`) reads its file, compresses it with `miniz_oxide`'s deflate (level
//! 6) and writes the result, the same code on both systems; the files are in
//! the directory `files` measures in memory. `work.compress_inproc` is the
//! same compression in this one process with no spawn and no file, so the
//! difference between the two is what the system adds.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::files::Scratch;
use crate::report::{refuse, result};
use crate::ns_since;

const FILES: usize = 256;
pub const FILE_BYTES: usize = 32 << 10;

/// One deterministic file of C-like text: words from a fixed vocabulary in
/// an order an LCG picks, so it compresses as source does rather than as
/// noise or as zeros.
pub fn source(seed: u64) -> Vec<u8> {
    const WORDS: [&str; 24] = [
        "int", "return", "if", "else", "for", "while", "struct", "static", "const", "char", "void", "size_t",
        "(", ")", "{", "}", ";", "=", "+", "*", "->", "0", "1", "\n",
    ];
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut out = Vec::with_capacity(FILE_BYTES);
    while out.len() < FILE_BYTES {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let word = WORDS[(x >> 33) as usize % WORDS.len()];
        out.extend_from_slice(word.as_bytes());
        out.push(b' ');
    }
    out.truncate(FILE_BYTES);
    out
}

/// `osbench cc <in> <out>`: one "compile".
pub fn cc(args: &[String]) {
    let [input, output] = args else {
        eprintln!("osbench cc: wants <in> <out>");
        std::process::exit(2);
    };
    let text = fs::read(input).unwrap_or_else(|e| panic!("read {input}: {e}"));
    let packed = miniz_oxide::deflate::compress_to_vec(&text, 6);
    fs::write(output, packed).unwrap_or_else(|e| panic!("write {output}: {e}"));
}

pub fn suite() {
    let memory = crate::files::memory().dir;
    let dir = match Scratch::new(&memory) {
        Ok(d) => d,
        Err(e) => return refuse("work", format!("no scratch directory under {memory}: {e}")),
    };
    let inputs: Vec<PathBuf> = (0..FILES).map(|i| dir.at(&format!("src{i:03}.c"))).collect();
    for (i, path) in inputs.iter().enumerate() {
        if let Err(e) = fs::write(path, source(i as u64)) {
            return refuse("work", format!("the sources: {e}"));
        }
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse("work", format!("no path to this program: {e}")),
    };

    // Every source compressed in this process, one after another: ns for the
    // set, five rounds after one.
    let texts: Vec<Vec<u8>> = (0..FILES).map(|i| source(i as u64)).collect();
    let inproc = || {
        let t = Instant::now();
        let packed: usize = texts.iter().map(|t| miniz_oxide::deflate::compress_to_vec(t, 6).len()).sum();
        std::hint::black_box(packed);
        ns_since(t)
    };
    inproc();
    crate::clock::settle();
    let mut ns: Vec<u64> = (0..5).map(|_| inproc()).collect();
    result("work.compress_inproc", "ns", &mut ns, &[("files", FILES.to_string()), ("file_bytes", FILE_BYTES.to_string())]);

    // The set built by a child per file, `jobs` at once: ns for the set,
    // five rounds after one, at one job and at one per CPU.
    for jobs in [1, crate::cpus()] {
        let bench = format!("work.build_shaped.j{jobs}");
        let span = crate::ran::start();
        let build = || -> Result<u64, String> {
            let t = Instant::now();
            let mut running: Vec<Child> = Vec::new();
            for (i, input) in inputs.iter().enumerate() {
                if running.len() == jobs {
                    let child = running.remove(0);
                    finish(child)?;
                }
                let output = dir.at(&format!("obj{i:03}.o"));
                let child = Command::new(&exe)
                    .arg("cc")
                    .arg(input)
                    .arg(&output)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("spawn a compile: {e}"))?;
                running.push(child);
            }
            for child in running {
                finish(child)?;
            }
            Ok(ns_since(t))
        };
        let rounds = || -> Result<Vec<u64>, String> {
            build()?;
            crate::clock::settle();
            (0..5).map(|_| build()).collect()
        };
        match rounds() {
            Ok(mut ns) => result(
                &bench,
                "ns",
                &mut ns,
                &[
                    ("files", FILES.to_string()),
                    ("jobs", jobs.to_string()),
                    ("parent", crate::ran::thread_over(span, crate::sys::tid())),
                ],
            ),
            Err(why) => refuse(&bench, why),
        }
    }
}

fn finish(mut child: Child) -> Result<(), String> {
    let status = child.wait().map_err(|e| format!("wait for a compile: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("a compile ended {status}")) }
}
