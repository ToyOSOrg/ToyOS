//! `usbd_spare`'s `unhanded` arm as a binary of its own, for a metal job, which
//! takes no argument: see `src/usbd_spare.rs`.

#[path = "../usbd_spare.rs"]
#[allow(dead_code, reason = "this binary runs one of the module's arms")]
mod usbd_spare;

fn main() {
    usbd_spare::unhanded(&usbd_spare::cap());
}
