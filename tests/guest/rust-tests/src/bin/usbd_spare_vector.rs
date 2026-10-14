//! `usbd_spare`'s `vector` arm as a binary of its own, for a metal job, which
//! takes no argument: see `src/usbd_spare.rs`.

#[path = "../usbd_spare.rs"]
#[allow(dead_code, reason = "this binary runs one of the module's arms")]
mod usbd_spare;

fn main() {
    usbd_spare::vector(&usbd_spare::cap());
}
