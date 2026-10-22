//! What a raw `SYS_SPAWN` names as its program and what a raw `SYS_OPEN`
//! opens, now that the kernel opens no file of `/system`: a program read whole
//! into a memory object of this process's own, and a file copied to the
//! kernel's `/tmp`.

// Each binary takes the half it needs.
#![allow(dead_code)]

use std::io::Read as _;

use toyos::process::Image;

/// `path`, read whole into a memory object of this process's own, which a
/// `SpawnArgs` names by [`Image::spawn_words`].
pub fn image_of(path: &str) -> Image {
    let mut file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().unwrap_or_else(|e| panic!("stat {path}: {e}")).len();
    Image::read(len, |buf| file.read(buf)).unwrap_or_else(|why| panic!("read {path} into an image: {why:?}"))
}

/// `path` copied to `/tmp` under its own name, which the kernel opens: the
/// path to open.
pub fn in_tmp(path: &str) -> String {
    let name = path.rsplit('/').next().expect("a path names a file");
    let copy = format!("/tmp/{name}");
    std::fs::copy(path, &copy).unwrap_or_else(|e| panic!("copy {path} to {copy}: {e}"));
    copy
}
