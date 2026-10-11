mod arch;
mod debug_refused;
mod preempt;
mod unmap_touch;

use arch::{first_entry, fp_isolation};

fn main() {
    let mut args = std::env::args();
    let invoked_as = args.next().expect("argv[0] names the probe");
    let args: Vec<String> = args.collect();
    match std::path::Path::new(&invoked_as).file_name().and_then(|n| n.to_str()) {
        Some("debug_refused") => debug_refused::main(args),
        Some("first_entry") => first_entry::main(args),
        Some("fp_isolation") => fp_isolation::main(args),
        Some("preempt") => preempt::main(args),
        Some("unmap_touch") => unmap_touch::main(args),
        other => panic!("kernelprobe: no probe is called {other:?}"),
    }
}
