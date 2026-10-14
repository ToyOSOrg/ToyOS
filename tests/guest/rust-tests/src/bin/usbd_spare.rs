//! `usbd_spare` with its arm named by its argument, for a QEMU boot that
//! runs several in one: see `src/usbd_spare.rs`. A metal job takes no
//! argument, so each arm a metal row needs is a binary of its own beside
//! this one.

#[path = "../usbd_spare.rs"]
mod usbd_spare;

fn main() {
    let cap = usbd_spare::cap();
    match std::env::args().nth(1).as_deref() {
        None => usbd_spare::drive(&cap),
        Some("fault") => usbd_spare::fault(&cap),
        Some("refused") => usbd_spare::refused(&cap),
        Some("vector") => usbd_spare::vector(&cap),
        Some("refused_unremapped") => usbd_spare::refused_unremapped(&cap),
        Some(other) => panic!("usbd_spare: no arm {other:?}"),
    }
}
