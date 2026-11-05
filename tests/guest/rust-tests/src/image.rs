//! Read a served executable into a memory object, and lay it out as the
//! ranges a raw spawn names (`toyos::elf::program`).

// Each binary takes the half it needs.
#![allow(dead_code)]

use std::io::Read as _;

use toyos::elf::program::{lay_out, Program, BASE};
use toyos::elf::Machine;
use toyos::process::Image;

/// `path`, read whole into a memory object of this process's own, which a
/// spawn's ranges name ([`laid`]).
pub fn image_of(path: &str) -> Image {
    let mut file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().unwrap_or_else(|e| panic!("stat {path}: {e}")).len();
    Image::read(len, |buf| file.read(buf)).unwrap_or_else(|why| panic!("read {path} into an image: {why:?}"))
}

/// `path` copied to the memory filesystem under its own name.
pub fn in_tmp(path: &str) -> String {
    let name = path.rsplit('/').next().expect("a path names a file");
    let copy = format!("/tmp/{name}");
    std::fs::copy(path, &copy).unwrap_or_else(|e| panic!("copy {path} to {copy}: {e}"));
    copy
}

/// `image` laid out as the SDK lays out a spawn's program: a raw `SpawnArgs`
/// names its ranges (`maps_ptr`, `maps_count`), `entry`, `image_base` and
/// build-id.
pub fn laid(image: &Image) -> Program {
    lay_out(image.bytes(), Machine::RUNNING, image.handle(), BASE).unwrap_or_else(|why| panic!("lay out an image: {why}"))
}
