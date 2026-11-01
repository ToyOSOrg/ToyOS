//! The display driver on `display-output`, one at a time: its connection's
//! IO. Every decision about it is `toyos_display_output::Output`'s.
//!
//! The compositor never waits on it. Each present is one `try_send`. A
//! refusal, a frame out of protocol or a hello that does not come within
//! [`HANDSHAKE_TIMEOUT`] drops the driver by name, and the desktop goes on
//! without it.

use std::time::Instant;

use toyos::ipc::{FrameRx, RxStep};
use toyos::Connection;
use toyos_desktop::{Mode, Rect};
use toyos_display_output::{Ended, Output, Present, Received, Size, MAX_FRAME, MSG_PRESENT};

use crate::client::HANDSHAKE_TIMEOUT;

/// The most frames one turn reads off the driver; it sends one per present.
const TURN_FRAMES: usize = 4;

pub struct Driver {
    conn: Connection,
    rx: FrameRx<MAX_FRAME>,
    epoch: Instant,
    output: Output,
}

impl Driver {
    pub fn new(conn: Connection, epoch: Instant) -> Self {
        let deadline = (Instant::now() + HANDSHAKE_TIMEOUT).duration_since(epoch).as_nanos() as u64;
        Self { conn, rx: FrameRx::new(), epoch, output: Output::new(deadline) }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    fn now(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// Whether its hello is overdue.
    pub fn overdue(&self) -> bool {
        self.output.overdue(self.now())
    }

    /// Whatever the driver sent, each frame whole; `Err` names why it is
    /// dropped. Its hello must offer the output the desktop is drawn on,
    /// `mode`.
    pub fn read(&mut self, mode: Mode) -> Result<(), String> {
        let drawn = Size { width: mode.width, height: mode.height };
        for _ in 0..TURN_FRAMES {
            let (msg_type, payload_len, handles) = match self.rx.pump(&self.conn) {
                RxStep::Idle => return Ok(()),
                RxStep::Eof => return Err("its connection ended".into()),
                RxStep::Malformed => return Err("a frame no protocol here can make".into()),
                RxStep::Frame { msg_type, payload_len, handles } => (msg_type, payload_len, handles),
            };
            let now = self.now();
            match self.output.received(now, msg_type, self.rx.payload(payload_len), handles, drawn) {
                Ok(Received::Offered { first, .. }) => {
                    println!("compositor: a display driver scans out the {mode} output");
                    send(&self.conn, &first)?;
                }
                Ok(Received::Answered { first, presented, next }) => {
                    if first {
                        println!(
                            "compositor: the display driver answered its first present, refreshing every {} ns",
                            presented.refresh_interval_nanos
                        );
                    }
                    if let Some(present) = next {
                        send(&self.conn, &present)?;
                    }
                }
                Err(ended) => return Err(said(ended)),
            }
        }
        Ok(())
    }

    /// `regions` of the output were composed: presented now, or merged into
    /// the present after the one outstanding.
    pub fn composed(&mut self, regions: &[Rect]) -> Result<(), String> {
        let Some(size) = self.output.size() else { return Ok(()) };
        let rects = regions.iter().filter_map(|r| {
            let r = r.intersect(Rect::new(0, 0, size.width as i32, size.height as i32));
            if r.is_empty() {
                return None;
            }
            let at = |v: i32| u32::try_from(v).expect("a rectangle clipped to the output is inside it");
            Some(toyos_display_output::Rect { x: at(r.x0), y: at(r.y0), width: at(r.w()), height: at(r.h()) })
        });
        match self.output.composed(rects) {
            Some(present) => send(&self.conn, &present),
            None => Ok(()),
        }
    }
}

fn said(ended: Ended) -> String {
    match ended {
        Ended::Late => format!("its hello did not come within {HANDSHAKE_TIMEOUT:?}"),
        Ended::Size { offered, drawn } => {
            format!("it offers {}x{}, and the desktop is drawn at {}x{}", offered.width, offered.height, drawn.width, drawn.height)
        }
        Ended::Frame(msg_type) => format!("frame {msg_type}, which `display-output` has not"),
        Ended::OutOfState(msg_type) => format!("frame {msg_type}, which its state has not"),
        Ended::Unasked(unasked) => match unasked.outstanding {
            Some(outstanding) => format!("it answered present {} while {outstanding} was outstanding", unasked.sequence),
            None => format!("it answered present {} with none outstanding", unasked.sequence),
        },
    }
}

fn send(conn: &Connection, present: &Present) -> Result<(), String> {
    let mut frame = [0u8; MAX_FRAME];
    let n = present.encode(&mut frame);
    conn.try_send_bytes(MSG_PRESENT, &frame[..n]).map_err(|why| format!("present {} refused ({why:?})", present.sequence))
}
