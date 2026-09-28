//! Needs a live compositor and a host that types GUI+V, which the shared boot
//! does not have — it is in `RUST_SKIP` and `metal_sim_hostile_clipboard` runs
//! it on the metal-sim profile.
//!
//! 1. **A wrong-typed handle.** A pipe end rides the retired region message.
//!    The kernel ends whoever maps a pipe as shared memory, so the compositor
//!    must refuse the client without ever receiving the handle — and the pipe's
//!    writer, queued on the refused connection, must go back to the kernel
//!    unused.
//! 2. **A copy that is not UTF-8.** The client commits a region of `0xFF`, and
//!    a paste has to be the clipboard from before it.
//! 3. **A region rewritten after its commit.** Once the compositor has closed
//!    the connection the region is the client's own again, and a paste has to
//!    be what was committed, not what the region holds now.
//! 4. **A copy never committed.** The client holds its region and says nothing;
//!    the compositor has to drop it by name.
//! 5. **A second begin.** A connection holding a region may send its commit and
//!    nothing else, so a second `MSG_COPY_BEGIN` on it is refused rather than
//!    answered with another region.
//! 6. **A begin with bytes past its length.** Refused rather than answered.
//! 7. **A commit with a payload.** Refused, so a paste has to be the clipboard
//!    from before it and not the region's text.
//! 8. **A commit on a window.** A commit names a region held, so a window
//!    sending one loses its connection.
//!
//! Each case ends with a probe the compositor answers from its dispatch. The
//! host asserts what this side cannot see: no handle fault and no compositor
//! exit in the kernel's records, and the refusals named.

use std::sync::atomic::Ordering;

use toyos::endow;
use toyos::ipc;
use toyos::shm::SharedMemory;
use toyos::{AsHandle, Connection};
use toyos_abi::syscall;
use toyos_abi::RawHandle;
use window::{Event, Window};

// The wire as this client speaks it, spelled here rather than imported: the
// negative control builds this binary against a `window` that predates all
// four.
const RETIRED_CLIPBOARD_SET_SHM: u32 = 10;
const COPY_BEGIN: u32 = 13;
const COPY_COMMIT: u32 = 14;
const COPY_REGION: u32 = 13;
/// The longest copy the compositor makes a region for.
const COPY_LEN: usize = 2 * 1024 * 1024;

/// The line the host answers with GUI+V, once.
const PASTE_MARKER: &str = "===HOSTILE_CLIPBOARD_PASTE===";

const BEFORE: &str = "hostile clipboard: the text before";
const AFTER: &str = "hostile clipboard: the text after";

fn main() {
    // First, so it has the focus: GUI+V pastes into the focused window.
    waiting("the paste target", "its window");
    let mut target = Window::create_with_title(160, 120, "paste")
        .unwrap_or_else(|e| fail("the paste target", &format!("no window: {e}")));
    target.present();
    waiting("the clipboard to start from", "the compositor taking it");
    window::clipboard_set(BEFORE)
        .unwrap_or_else(|e| fail("the clipboard to start from", &e.to_string()));
    probe("the clipboard to start from");

    wrong_typed_handle();
    probe("a wrong-typed handle");

    let what = "a copy that is not UTF-8";
    commit_filled(what, 0xFF);
    probe(what);
    let first = paste(&mut target, what);
    if first != BEFORE.as_bytes() {
        let len = first.len();
        fail(what, &format!("the paste was {len} bytes, not the clipboard from before"));
    }

    let what = "a region rewritten after its commit";
    let region = commit_filled(what, b'C');
    // Text too, so a read of the region at the paste is pasted rather than
    // refused.
    fill(&region, b'D');
    probe(what);
    let second = paste(&mut target, what);
    if second.len() != COPY_LEN || second.iter().any(|&b| b != b'C') {
        fail(what, &describe(&second, b'C'));
    }

    let what = "a copy never committed";
    let (conn, _region) = begin_copy(what);
    await_hangup(conn.as_handle(), what, "the compositor giving up on the commit");
    probe(what);

    let what = "a second begin on a copy";
    let (conn, _region) = begin_copy(what);
    conn.send(COPY_BEGIN, &window::ClipboardShmMsg { len: COPY_LEN as u32 })
        .unwrap_or_else(|e| fail(what, &format!("could not begin again: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor's refusal");
    probe(what);

    let what = "a begin with bytes past its length";
    let conn = connect(what);
    let mut begin = (COPY_LEN as u32).to_ne_bytes().to_vec();
    begin.extend_from_slice(&[0; 4]);
    conn.send_bytes(COPY_BEGIN, &begin)
        .unwrap_or_else(|e| fail(what, &format!("could not begin: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor's refusal");
    probe(what);

    let what = "a commit with a payload";
    set_inline(what, AFTER);
    let (conn, region) = begin_copy(what);
    fill(&region, b'E');
    conn.send_bytes(COPY_COMMIT, &[0; 4])
        .unwrap_or_else(|e| fail(what, &format!("no commit: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor's refusal");
    probe(what);
    let third = paste(&mut target, what);
    if third != AFTER.as_bytes() {
        let len = third.len();
        fail(what, &format!("the paste was {len} bytes, not the clipboard from before"));
    }

    // Last: the new window takes the focus the pastes went to.
    let what = "a commit on a window";
    waiting(what, "its window");
    let committing = Window::create_with_title(64, 64, "commit")
        .unwrap_or_else(|e| fail(what, &format!("no window: {e}")));
    ipc::signal(committing.handle(), COPY_COMMIT)
        .unwrap_or_else(|e| fail(what, &format!("no commit: {e:?}")));
    ipc::signal(committing.handle(), window::MSG_GET_RESOLUTION)
        .unwrap_or_else(|e| fail(what, &format!("no probe: {e:?}")));
    await_hangup(committing.handle(), what, "the compositor closing the window");
    probe(what);

    println!("hostile clipboard: every case survived, compositor still serving");
}

/// Put `text` on the clipboard inline, and wait for the compositor to be done
/// with it.
fn set_inline(what: &str, text: &str) {
    let conn = connect(what);
    conn.send_bytes(window::MSG_CLIPBOARD_SET, text.as_bytes())
        .unwrap_or_else(|e| fail(what, &format!("could not set the clipboard: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor closing the clipboard");
}

/// A pipe end where the retired message carried a region.
fn wrong_typed_handle() {
    let what = "a wrong-typed handle";
    let ends = syscall::pipe().unwrap_or_else(|e| fail(what, &format!("no pipe: {e:?}")));
    let conn = connect(what);
    conn.send_with_handles(
        &[ends.write],
        RETIRED_CLIPBOARD_SET_SHM,
        &window::ClipboardShmMsg { len: 64 },
    )
    .unwrap_or_else(|e| fail(what, &format!("could not send: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor's refusal");
    // The send moved the only writer, so the reader hangs up exactly when the
    // queue holding it is gone — and not while anything holds it.
    await_hangup(ends.read, what, "the writer's return to the kernel");
    syscall::close(ends.read);
}

/// Commit a whole copy of `byte`, and wait for the compositor to be done with
/// it.
fn commit_filled(what: &str, byte: u8) -> SharedMemory {
    let (conn, region) = begin_copy(what);
    fill(&region, byte);
    conn.signal(COPY_COMMIT).unwrap_or_else(|e| fail(what, &format!("no commit: {e:?}")));
    await_hangup(conn.as_handle(), what, "the compositor closing the copy");
    region
}

/// A connection holding the region the compositor made for a whole copy.
fn begin_copy(what: &str) -> (Connection, SharedMemory) {
    let conn = connect(what);
    conn.send(COPY_BEGIN, &window::ClipboardShmMsg { len: COPY_LEN as u32 })
        .unwrap_or_else(|e| fail(what, &format!("could not begin: {e:?}")));
    waiting(what, "the compositor's region");
    let header = conn.recv_header().unwrap_or_else(|e| fail(what, &format!("no answer: {e:?}")));
    if header.msg_type != COPY_REGION || header.len() != 0 {
        fail(
            what,
            &format!(
                "the compositor answered with message type {} and {} bytes",
                header.msg_type,
                header.len()
            ),
        );
    }
    let [region] =
        conn.recv_handles_exact::<1>().unwrap_or_else(|| fail(what, "the answer had no region"));
    let region = SharedMemory::adopt(region, COPY_LEN)
        .unwrap_or_else(|e| fail(what, &format!("the region would not map: {e:?}")));
    (conn, region)
}

fn fill(region: &SharedMemory, byte: u8) {
    for b in region.as_atomic() {
        b.store(byte, Ordering::Relaxed);
    }
}

/// Ask the host for GUI+V and return what the target is pasted.
fn paste(target: &mut Window, what: &str) -> Vec<u8> {
    waiting(what, "the paste");
    println!("{PASTE_MARKER}");
    loop {
        match target.recv_event() {
            Event::ClipboardPaste(text) => return text,
            Event::Close => fail(what, "the paste target's window was closed"),
            _ => {}
        }
    }
}

/// A paste, summarised — never printed whole.
fn describe(text: &[u8], fill: u8) -> String {
    let stray = text.iter().position(|&b| b != fill);
    format!(
        "the paste was {} bytes, UTF-8: {}, first byte that is not {:?} at {stray:?}",
        text.len(),
        std::str::from_utf8(text).is_ok(),
        fill as char
    )
}

fn connect(what: &str) -> Connection {
    endow::service("compositor")
        .unwrap_or_else(|e| fail(what, &format!("the compositor is not serving: {e:?}")))
}

/// Said before every blocking wait: the line a missing event leaves last.
fn waiting(what: &str, awaited: &str) {
    println!("hostile clipboard: [{what}] waiting for {awaited}");
}

/// Wait for the peer of `handle` to hang up, failing on anything it sends.
fn await_hangup(handle: RawHandle, what: &str, awaited: &str) {
    waiting(what, awaited);
    let mut byte = [0u8; 1];
    match syscall::read(handle, &mut byte) {
        Ok(0) => {}
        Ok(_) => fail(what, &format!("the peer answered where {awaited} was due")),
        Err(e) => fail(what, &format!("waiting for {awaited}: {e:?}")),
    }
}

/// Ask the compositor something it always answers.
fn probe(what: &str) {
    let conn = connect(what);
    conn.signal(window::MSG_GET_RESOLUTION)
        .unwrap_or_else(|e| fail(what, &format!("could not ask for the resolution: {e:?}")));
    waiting(what, "the compositor's answer to a probe");
    let header = conn
        .recv_header()
        .unwrap_or_else(|e| fail(what, &format!("the probe went unanswered: {e:?}")));
    if header.msg_type != window::MSG_RESOLUTION_CHANGED {
        fail(what, &format!("the probe was answered with message type {}", header.msg_type));
    }
}

fn fail(what: &str, msg: &str) -> ! {
    eprintln!("hostile clipboard: [{what}] {msg}");
    std::process::exit(1);
}
