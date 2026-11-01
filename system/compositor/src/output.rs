//! The display driver on `display-output` (`toyos_display_output`), one at a
//! time: told every frame composed into the output it scans out, and where.
//!
//! The compositor never waits on it. Each present is one `try_send`. A
//! refusal, a frame out of protocol or a hello that does not come within
//! [`HANDSHAKE_TIMEOUT`] drops the driver by name, and the desktop goes on
//! without it.

use std::time::Instant;

use toyos::ipc::{FrameRx, RxStep};
use toyos::Connection;
use toyos_desktop::{Mode, Rect};
use toyos_display_output::{FromDriver, Present, Presenting, Size, MAX_FRAME, MSG_PRESENT};

use crate::client::HANDSHAKE_TIMEOUT;

/// The most frames one turn reads off the driver; it sends one per present.
const TURN_FRAMES: usize = 4;

pub struct Driver {
    conn: Connection,
    rx: FrameRx<MAX_FRAME>,
    since: Instant,
    serving: Option<Serving>,
}

struct Serving {
    size: Size,
    presenting: Presenting,
    answered: u64,
}

impl Driver {
    pub fn new(conn: Connection) -> Self {
        Self { conn, rx: FrameRx::new(), since: Instant::now(), serving: None }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Whether its hello is overdue.
    pub fn overdue(&self, now: Instant) -> bool {
        self.serving.is_none() && now.duration_since(self.since) >= HANDSHAKE_TIMEOUT
    }

    /// Whatever the driver sent, each frame whole; `Err` names why it is
    /// dropped. Its hello must offer the output the desktop is drawn on,
    /// `mode`.
    pub fn read(&mut self, mode: Mode) -> Result<(), String> {
        for _ in 0..TURN_FRAMES {
            let (msg_type, payload_len) = match self.rx.pump(&self.conn) {
                RxStep::Idle => return Ok(()),
                RxStep::Eof => return Err("its connection ended".into()),
                RxStep::Malformed => return Err("a frame no protocol here can make".into()),
                RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
            };
            match (FromDriver::decode(msg_type, self.rx.payload(payload_len)), &mut self.serving) {
                (Some(FromDriver::Hello(hello)), None) => {
                    if (hello.size.width, hello.size.height) != (mode.width, mode.height) {
                        return Err(format!("it offers {}x{}, and the desktop is drawn at {mode}", hello.size.width, hello.size.height));
                    }
                    println!("compositor: a display driver scans out the {mode} output");
                    self.serving = Some(Serving { size: hello.size, presenting: Presenting::new(), answered: 0 });
                }
                (Some(FromDriver::Presented(presented)), Some(serving)) => {
                    let next = serving.presenting.presented(&presented).map_err(|unasked| format!("{unasked:?}"))?;
                    serving.answered += 1;
                    if serving.answered == 1 {
                        println!(
                            "compositor: the display driver answered its first present, refreshing every {} ns",
                            presented.refresh_interval_nanos
                        );
                    }
                    if let Some(present) = next {
                        send(&self.conn, &present)?;
                    }
                }
                _ => return Err(format!("frame {msg_type}, which its state has not")),
            }
        }
        Ok(())
    }

    /// `regions` of the output were composed: presented now, or merged into
    /// the present after the one outstanding.
    pub fn composed(&mut self, regions: &[Rect]) -> Result<(), String> {
        let Some(serving) = &mut self.serving else { return Ok(()) };
        let size = serving.size;
        let rects = regions.iter().filter_map(|r| {
            let r = r.intersect(Rect::new(0, 0, size.width as i32, size.height as i32));
            if r.is_empty() {
                return None;
            }
            let at = |v: i32| u32::try_from(v).expect("a rectangle clipped to the output is inside it");
            Some(toyos_display_output::Rect { x: at(r.x0), y: at(r.y0), width: at(r.w()), height: at(r.h()) })
        });
        match serving.presenting.damaged(rects) {
            Some(present) => send(&self.conn, &present),
            None => Ok(()),
        }
    }
}

fn send(conn: &Connection, present: &Present) -> Result<(), String> {
    let mut frame = [0u8; MAX_FRAME];
    let n = present.encode(&mut frame);
    conn.try_send_bytes(MSG_PRESENT, &frame[..n]).map_err(|why| format!("present {} refused ({why:?})", present.sequence))
}
