//! Pointer reports: the boot mouse (HID 1.11 Appendix B.2) and QEMU's
//! `usb-tablet`.
//!
//! ```text
//! mouse   byte 0 buttons (bit 0 left, 1 right, 2 middle, the rest the maker's)
//!         byte 1 X, byte 2 Y: signed displacements, Y positive towards the user
//!         byte 3 the wheel, where the mouse sends one (not B.2's; see below)
//! tablet  byte 0 buttons, bytes 1-2 X and 3-4 Y little endian, 0 to 0x7FFF,
//!         byte 5 the wheel
//! ```
//!
//! Buttons are carried whole: the bits past the boot three are further
//! buttons on every pointer this decoder reads, QEMU's included.
//!
//! Appendix B.2 lays out bytes 0 to 2 and leaves the rest to the device.
//! Byte 3 is read as the wheel because QEMU's `usb-mouse` report descriptor
//! puts it there and Linux's boot mouse driver `usbmouse` reads it so; a
//! mouse that puts something else there scrolls.

/// The largest absolute coordinate, which is the tablet's logical maximum and
/// the edge of the space every pointer's motion is merged into.
pub const MOST: u16 = 0x7FFF;

/// Which report an interface sends, fixed by its protocol when it binds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pointer {
    Mouse,
    Tablet,
}

/// Where a report moves the pointer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Relative { dx: i8, dy: i8 },
    Absolute { x: u16, y: u16 },
}

/// One report, read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Report {
    pub buttons: u8,
    pub motion: Motion,
    pub wheel: i8,
}

/// Why a report was not believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Too short for its layout, or longer than its transfer asks for.
    Length(usize),
    /// A tablet position past [`MOST`]: off the surface.
    OffTheSurface { x: u16, y: u16 },
}

impl Pointer {
    /// What the interrupt transfer asks for: the longest report this layout
    /// reads.
    pub const fn request(self) -> usize {
        match self {
            Self::Mouse => 4,
            Self::Tablet => 6,
        }
    }

    /// `report`, as this layout reads it, or why it does not.
    pub fn decode(self, report: &[u8]) -> Result<Report, Refused> {
        match (self, report) {
            (Self::Mouse, &[buttons, dx, dy]) => Ok(Report {
                buttons,
                motion: Motion::Relative { dx: dx as i8, dy: dy as i8 },
                wheel: 0,
            }),
            (Self::Mouse, &[buttons, dx, dy, wheel]) => Ok(Report {
                buttons,
                motion: Motion::Relative { dx: dx as i8, dy: dy as i8 },
                wheel: wheel as i8,
            }),
            (Self::Tablet, &[buttons, x0, x1, y0, y1, wheel]) => {
                let (x, y) = (u16::from_le_bytes([x0, x1]), u16::from_le_bytes([y0, y1]));
                if x > MOST || y > MOST {
                    return Err(Refused::OffTheSurface { x, y });
                }
                Ok(Report { buttons, motion: Motion::Absolute { x, y }, wheel: wheel as i8 })
            }
            (_, report) => Err(Refused::Length(report.len())),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::keyboard::tests::Bytes;
    use std::vec::Vec;

    fn mouse(buttons: u8, dx: i8, dy: i8, wheel: i8) -> Result<Report, Refused> {
        Ok(Report { buttons, motion: Motion::Relative { dx, dy }, wheel })
    }

    fn tablet(buttons: u8, x: u16, y: u16, wheel: i8) -> Result<Report, Refused> {
        Ok(Report { buttons, motion: Motion::Absolute { x, y }, wheel })
    }

    /// Appendix B.2: displacements are signed bytes, so 0x80 is -128 and not
    /// 128, and a mouse with no wheel sends three bytes.
    #[test]
    fn a_mouse_report_is_read_as_appendix_b_lays_it_out() {
        assert_eq!(Pointer::Mouse.decode(&[0x01, 0x05, 0xFB]), mouse(0x01, 5, -5, 0));
        assert_eq!(Pointer::Mouse.decode(&[0x07, 0x80, 0x7F, 0xFF]), mouse(0x07, -128, 127, -1));
        assert_eq!(Pointer::Mouse.decode(&[0xF8, 0, 0, 1]), mouse(0xF8, 0, 0, 1), "every button bit is carried");
    }

    #[test]
    fn a_tablet_report_is_its_position_little_endian() {
        assert_eq!(Pointer::Tablet.decode(&[0x02, 0x34, 0x12, 0xFF, 0x7F, 0xFF]), tablet(0x02, 0x1234, 0x7FFF, -1));
        assert_eq!(Pointer::Tablet.decode(&[0, 0, 0, 0, 0, 0]), tablet(0, 0, 0, 0));
    }

    #[test]
    fn a_tablet_position_off_the_surface_is_refused() {
        assert_eq!(
            Pointer::Tablet.decode(&[0, 0x00, 0x80, 0, 0, 0]),
            Err(Refused::OffTheSurface { x: 0x8000, y: 0 })
        );
        assert_eq!(
            Pointer::Tablet.decode(&[0, 0, 0, 0xFF, 0xFF, 0]),
            Err(Refused::OffTheSurface { x: 0, y: 0xFFFF })
        );
    }

    /// A short transfer is not the report with its tail as it was: those bytes
    /// are the last report's.
    #[test]
    fn a_report_of_a_length_its_layout_does_not_have_is_refused() {
        for len in 0..=16 {
            let report = std::vec![0u8; len];
            let mouse = Pointer::Mouse.decode(&report);
            assert_eq!(mouse.is_ok(), len == 3 || len == 4, "mouse, {len} bytes: {mouse:?}");
            let tablet = Pointer::Tablet.decode(&report);
            assert_eq!(tablet.is_ok(), len == 6, "tablet, {len} bytes: {tablet:?}");
            if len > Pointer::Mouse.request() || len < 3 {
                assert_eq!(mouse, Err(Refused::Length(len)));
            }
            if len != Pointer::Tablet.request() {
                assert_eq!(tablet, Err(Refused::Length(len)));
            }
        }
    }

    /// Every report QEMU 11.1's `usb-mouse` and `usb-tablet` delivered to the
    /// kernel's driver over xHCI, each transfer whole, for QMP events: a move
    /// of (5, -5), one of (-200, 200), which QEMU splits at a byte's range,
    /// the left button down and up, a wheel notch, and the tablet at
    /// (0x1234, 0x7FFF) and (0, 0).
    #[test]
    fn reports_qemu_sent_read_as_the_motion_that_was_sent() {
        use Pointer::{Mouse, Tablet};
        let captured: &[(Pointer, &[u8], Result<Report, Refused>)] = &[
            (Mouse, &[0x00, 0x05, 0xFB, 0x00], mouse(0, 5, -5, 0)),
            (Mouse, &[0x00, 0x81, 0x7F, 0x00], mouse(0, -127, 127, 0)),
            (Mouse, &[0x00, 0xB7, 0x49, 0x00], mouse(0, -73, 73, 0)),
            (Mouse, &[0x01, 0x00, 0x00, 0x00], mouse(1, 0, 0, 0)),
            (Mouse, &[0x00, 0x00, 0x00, 0x00], mouse(0, 0, 0, 0)),
            (Mouse, &[0x00, 0x00, 0x00, 0x01], mouse(0, 0, 0, 1)),
            (Tablet, &[0x00, 0x34, 0x12, 0xFF, 0x7F, 0x00], tablet(0, 0x1234, 0x7FFF, 0)),
            (Tablet, &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00], tablet(0, 0, 0, 0)),
        ];
        for &(pointer, report, want) in captured {
            assert_eq!(pointer.decode(report), want, "{pointer:?} {report:02x?}");
        }
    }

    /// Every report is read or refused by name, and a read one says what its
    /// bytes say: a tablet's position is the bytes it came in, and never past
    /// the surface.
    #[test]
    fn every_report_is_read_or_refused_and_nothing_else() {
        const EDGES: &[u8] = &[0, 1, 0x7F, 0x80, 0x81, 0xFE, 0xFF];
        let mut bytes = Bytes::new(0x706f_696e_7465);
        for case in 0..200_000 {
            let len = (bytes.next() % 9) as usize;
            let report: Vec<u8> = (0..len).map(|_| bytes.byte(EDGES)).collect();
            for pointer in [Pointer::Mouse, Pointer::Tablet] {
                match pointer.decode(&report) {
                    Ok(read) => {
                        assert!(len <= pointer.request(), "case {case}: {pointer:?} read {len} bytes");
                        assert_eq!(read.buttons, report[0], "case {case}");
                        match read.motion {
                            Motion::Relative { dx, dy } => {
                                assert_eq!(pointer, Pointer::Mouse, "case {case}");
                                assert_eq!((dx as u8, dy as u8), (report[1], report[2]), "case {case}");
                            }
                            Motion::Absolute { x, y } => {
                                assert_eq!(pointer, Pointer::Tablet, "case {case}");
                                assert!(x <= MOST && y <= MOST, "case {case}: ({x}, {y})");
                                assert_eq!(x.to_le_bytes(), [report[1], report[2]], "case {case}");
                                assert_eq!(y.to_le_bytes(), [report[3], report[4]], "case {case}");
                            }
                        }
                    }
                    Err(Refused::Length(n)) => assert_eq!(n, len, "case {case}"),
                    Err(Refused::OffTheSurface { x, y }) => {
                        assert_eq!(pointer, Pointer::Tablet, "case {case}");
                        assert!(x > MOST || y > MOST, "case {case}: ({x}, {y}) refused");
                    }
                }
            }
        }
    }
}
