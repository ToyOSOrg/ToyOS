//! memmap2 on ToyOS refuses an executable view by name: a heap buffer is not
//! executable, so `make_exec` answers `Unsupported` as `map_exec` does, rather
//! than an `Ok` a caller would then jump into.

use std::io::ErrorKind;

use memmap2::MmapOptions;

fn main() {
    let anon = MmapOptions::new().len(4096).map_anon().expect("an anonymous map is a zeroed buffer");
    match anon.make_exec() {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("make_exec refused: {e}"),
        Err(e) => panic!("make_exec refused with {e:?}, not Unsupported"),
        Ok(_) => panic!("make_exec answered Ok on a heap buffer"),
    }
}
