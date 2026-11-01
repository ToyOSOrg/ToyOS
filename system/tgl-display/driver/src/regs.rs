//! The registers this driver touches, from IHD-OS-TGL-Vol 2c-12.21, and their
//! fields. Pipe `p` (A = 0) is `p * 1000h` above pipe A's; plane `n` (1-7)
//! of a pipe is `(n - 1) * 100h` above plane 1's.

/// Pipes A-D.
pub const PIPES: usize = 4;
/// Universal planes per pipe.
pub const PLANES: usize = 7;
/// Watermark latency levels.
pub const LEVELS: usize = 8;

const PIPE_STRIDE: usize = 0x1000;
const PLANE_STRIDE_BYTES: usize = 0x100;

/// TRANS_CONF_A, 70008h: bit 30 Transcoder State.
pub fn trans_conf(pipe: usize) -> usize {
    0x7_0008 + pipe * PIPE_STRIDE
}
pub const TRANSCODER_STATE: u32 = 1 << 30;

/// TRANS_HTOTAL_A, 60000h: 29:16 Horizontal Total, in pixels minus one.
pub const TRANS_HTOTAL_A: usize = 0x6_0000;
/// TRANS_VTOTAL_A, 6000Ch: 29:16 Vertical Total, in lines minus one.
pub const TRANS_VTOTAL_A: usize = 0x6_000C;
pub const TOTAL_SHIFT: u32 = 16;
pub const TOTAL: u32 = 0x3FFF;

/// PIPE_FRMCNT_A, 70040h: frames started since the pipe was enabled.
pub const PIPE_FRMCNT_A: usize = 0x7_0040;

/// WM_LINETIME_A, 45270h: 8:0 line time in 0.125 µs.
pub const WM_LINETIME_A: usize = 0x4_5270;
pub const LINE_TIME: u32 = 0x1FF;

/// CUR_CTL_A, 70080h: 5:0 Cursor Mode Select, 0 for disabled.
pub const CUR_CTL_A: usize = 0x7_0080;
pub const CURSOR_MODE: u32 = 0x3F;

fn plane(pipe: usize, plane: usize, at: usize) -> usize {
    assert!(pipe < PIPES && (1..=PLANES).contains(&plane), "pipe {pipe} plane {plane}");
    at + pipe * PIPE_STRIDE + (plane - 1) * PLANE_STRIDE_BYTES
}

/// PLANE_CTL_1_A, 70180h: bit 31 Plane Enable, 27:23 Source Pixel Format,
/// 12:10 Tiled Surface.
pub fn plane_ctl(pipe: usize, n: usize) -> usize {
    plane(pipe, n, 0x7_0180)
}
pub const PLANE_ENABLE: u32 = 1 << 31;

/// PLANE_SIZE_1_A, 70190h: 28:16 height minus one, 12:0 width minus one.
pub fn plane_size(pipe: usize, n: usize) -> usize {
    plane(pipe, n, 0x7_0190)
}

/// PLANE_SURF_1_A, 7019Ch: the write that arms the plane's double-buffered
/// registers, its watermarks among them (Vol 12 "Watermark Algorithm" 5C).
pub fn plane_surf(pipe: usize, n: usize) -> usize {
    plane(pipe, n, 0x7_019C)
}

/// PLANE_WM_<level>_1_A, 70240h + 4 * level.
pub fn plane_wm(pipe: usize, n: usize, level: usize) -> usize {
    assert!(level < LEVELS);
    plane(pipe, n, 0x7_0240) + 4 * level
}

/// PLANE_BUF_CFG_1_A, 7027Ch: 26:16 Buffer End, 10:0 Buffer Start.
pub fn plane_buf_cfg(pipe: usize, n: usize) -> usize {
    plane(pipe, n, 0x7_027C)
}

/// PLANE_WM: bit 31 Enable, 18:14 Lines, 10:0 Blocks.
pub const WM_ENABLE: u32 = 1 << 31;

/// TRANS_DDI_FUNC_CTL_A, 60400h: 30:27 DDI Select, 1h-3h for DDI A-C.
pub const TRANS_DDI_FUNC_CTL_A: usize = 0x6_0400;
pub const DDI_SELECT_SHIFT: u32 = 27;
pub const DDI_SELECT: u32 = 0xF;
pub const DDI_A: u32 = 1;

/// AUDIO_PIN_BUF_CTL, 48414h: bit 31 Enable, the display audio pins' I/O
/// buffers.
pub const AUDIO_PIN_BUF_CTL: usize = 0x4_8414;
pub const AUDIO_BUFFER_ENABLE: u32 = 1 << 31;

/// VGA_CONTROL, 41000h: bit 31 VGA Display Disable.
pub const VGA_CONTROL: usize = 0x4_1000;
pub const VGA_DISPLAY_DISABLE: u32 = 1 << 31;

/// PWR_WELL_CTL1, 45400h, generally the BIOS's, and PWR_WELL_CTL2, 45404h,
/// generally the driver's: requests from every instance are ORed. Power well
/// `n` (1-5) has its request at bit `2n - 1` and its state at bit `2n - 2`.
pub const PWR_WELL_CTL1: usize = 0x4_5400;
pub const PWR_WELL_CTL2: usize = 0x4_5404;
pub fn power_well_request(well: u32) -> u32 {
    1 << (2 * well - 1)
}
pub fn power_well_state(well: u32) -> u32 {
    1 << (2 * well - 2)
}

/// DC_STATE_EN, 45504h: 1:0 Dynamic DC State Enable, 10b "Enable up to DC6".
/// Bits 9, 8 and 4 are hardware's and must not be changed.
pub const DC_STATE_EN: usize = 0x4_5504;
pub const DYNAMIC_DC_STATE: u32 = 0b11;
pub const UP_TO_DC6: u32 = 0b10;

/// 45520h, which Vol 12 "Sequence to Allow DC5 or DC6" step 3 names by address
/// alone: bits 1:0 set before DC5 or DC6 is allowed, and never cleared.
pub const DC_PREREQUISITE: usize = 0x4_5520;
pub const DC_PREREQUISITE_BITS: u32 = 0b11;

/// The GT driver mailbox (Vol 2c GTDRIVER_MAILBOX_*; fields from Vol 12
/// "Mailbox Commands" and "Sequence Before Frequency Change"): Run/Busy bit
/// 31, PARAM1 15:8, the command and its answered error code 7:0.
pub const MAILBOX_INTERFACE: usize = 0x13_8124;
pub const MAILBOX_DATA0: usize = 0x13_8128;
pub const MAILBOX_DATA1: usize = 0x13_812C;
pub const RUN_BUSY: u32 = 1 << 31;

/// TRANS_VBLANK_A, 60010h: 12:0 Vertical Blank Start, in lines minus one.
/// TRANS_VTOTAL_A's 12:0 is Vertical Active, in lines minus one.
pub const TRANS_VBLANK_A: usize = 0x6_0010;
pub const LINES: u32 = 0x1FFF;

/// PWR_WELL_CTL_AUX1, 45440h, generally the BIOS's, and PWR_WELL_CTL_AUX2,
/// 45444h, the driver's: bit 1 AUX A IO Power Request, bit 0 its state.
pub const PWR_WELL_CTL_AUX1: usize = 0x4_5440;
pub const PWR_WELL_CTL_AUX2: usize = 0x4_5444;
pub const AUX_A_REQUEST: u32 = 1 << 1;
pub const AUX_A_STATE: u32 = 1 << 0;

/// DDI_AUX_CTL_A, 64010h: 31 Send Busy, 30 Done, 28 Time out error, 27:26
/// Time out timer value, 25 Receive error, 24:20 Message Size, 9:5 Fast Wake
/// Sync Pulse Count, 4:0 Sync Pulse Count. DDI_AUX_DATA_A, five dwords from
/// 64014h, "The MSbyte is transmitted first".
pub const DDI_AUX_CTL_A: usize = 0x6_4010;
pub const DDI_AUX_DATA_A: usize = 0x6_4014;
pub const AUX_SEND_BUSY: u32 = 1 << 31;
pub const AUX_DONE: u32 = 1 << 30;
pub const AUX_TIMEOUT: u32 = 1 << 28;
pub const AUX_RECEIVE_ERROR: u32 = 1 << 25;
/// 11b, 4000 µs: Vol 12 "AUX Functional Sequence" step 4 asks for the
/// maximum.
pub const AUX_TIMEOUT_MAX: u32 = 0b11 << 26;
pub const AUX_SIZE_SHIFT: u32 = 20;
pub const AUX_SIZE: u32 = 0x1F;
/// "SW must always program this field to the value of decimal 7."
pub const AUX_FAST_WAKE_SYNC: u32 = 7 << 5;
/// At least 25 ("the minimum amount of pulses required for a standard
/// transaction"); 31 is 32 pulses.
pub const AUX_SYNC: u32 = 31;
/// The most bytes a message holds, header included.
pub const AUX_BYTES: usize = 20;

/// SRD_CTL_A, 60800h: 31 SRD Enable, 29 Context restore to PSR Active
/// (hardware's, kept), 24:20 Max Sleep Time, 11 TP2 TP3 Select, 9:8 TP2 TP3
/// Time, 7:6 TP4 time ("Always program TP4 to 11b"), 5:4 TP1 Time, 3:0 Idle
/// Frames.
pub const SRD_CTL_A: usize = 0x6_0800;
pub const SRD_ENABLE: u32 = 1 << 31;
pub const SRD_CONTEXT_RESTORE: u32 = 1 << 29;
/// All ones, about 3.875 s; all zeros is invalid.
pub const SRD_MAX_SLEEP: u32 = 0x1F << 20;
/// 10b, 2.5 ms, for TP1 and for TP2: the longest training a wake can need.
pub const SRD_TP2_TIME: u32 = 0b10 << 8;
pub const SRD_TP4_SKIP: u32 = 0b11 << 6;
pub const SRD_TP1_TIME: u32 = 0b10 << 4;
pub const SRD_IDLE_FRAMES: u32 = 0xF;

/// SRD_STATUS_A, 60840h: 31:29 SRD State, 27:26 Link Status, 19:16 SRD Entry
/// Count. SRD_PERF_CNT_A, 60844h, 23:0: milliseconds in SRD.
pub const SRD_STATUS_A: usize = 0x6_0840;
pub const SRD_PERF_CNT_A: usize = 0x6_0844;
pub const SRD_STATE_SHIFT: u32 = 29;

/// PSR_IMR_A, 60814h, and PSR_IIR_A, 60818h: 2 PSR Aux Error, 1 PSR Exit
/// ("set on the first blank start after PSR exit").
pub const PSR_IMR_A: usize = 0x6_0814;
pub const PSR_IIR_A: usize = 0x6_0818;
pub const PSR_AUX_ERROR: u32 = 1 << 2;
pub const PSR_EXIT: u32 = 1 << 1;

/// PSR_MASK_A, 60860h: 28 Mask Max Sleep, 26 Mask Memup, 25 Mask Hotplug.
/// None but 31:30 may change while PSR is enabled.
pub const PSR_MASK_A: usize = 0x6_0860;
pub const PSR_MASKED: u32 = 1 << 28 | 1 << 26 | 1 << 25;

/// PSR2_CTL_A, 60900h: bit 31 PSR2 Enable.
pub const PSR2_CTL_A: usize = 0x6_0900;
pub const PSR2_ENABLE: u32 = 1 << 31;

/// VIDEO_DIP_CTL_A, 60200h: 26:25 VSC select, 00b "Header and Data", and bit
/// 20 VDIP Enable VSC.
pub const VIDEO_DIP_CTL_A: usize = 0x6_0200;
pub const VSC_SELECT: u32 = 0b11 << 25;
pub const VSC_ENABLE: u32 = 1 << 20;

/// DE_PIPE_INTERRUPT_A (Vol 2c), 44400h-4440Fh: ISR, IMR, IIR and IER, bit 0
/// Vblank. The other categories this driver enables nothing in, at their IER:
/// pipes B-D, DE_PORT_INTERRUPT, DE_MISC_INTERRUPT, DE_HPD_INTERRUPT and
/// AUD_INTERRUPT. Each IMR is 8 below its IER: ISR, IMR, IIR and IER at +0,
/// +4, +8 and +Ch.
pub const PIPE_A_IMR: usize = 0x4_4404;
pub const PIPE_A_IIR: usize = 0x4_4408;
pub const PIPE_A_IER: usize = 0x4_440C;
pub const VBLANK: u32 = 1 << 0;
pub const UNUSED_IERS: [usize; 7] = [0x4_441C, 0x4_442C, 0x4_443C, 0x4_444C, 0x4_446C, 0x4_447C, 0x4_448C];

/// DISPLAY_INT_CTL, 44200h: bit 31 Display Interrupt Enable, 25:16 the
/// categories pending, pipe A's at 16.
pub const DISPLAY_INT_CTL: usize = 0x4_4200;
pub const DISPLAY_INTERRUPT_ENABLE: u32 = 1 << 31;
pub const PENDING: u32 = 0x3FF << 16;
pub const PIPE_A_PENDING: u32 = 1 << 16;

/// GFX_MSTR_INTR, 190010h: bit 31 Primary Interrupt, the control every
/// graphics interrupt needs to reach the PCI function's message; bit 16
/// Display.
pub const GFX_MSTR_INTR: usize = 0x19_0010;
pub const PRIMARY_INTERRUPT: u32 = 1 << 31;
pub const DISPLAY_PENDING: u32 = 1 << 16;

/// The highest offset this driver reaches, for the window's bound.
pub const SPAN: usize = GFX_MSTR_INTR + 4;
