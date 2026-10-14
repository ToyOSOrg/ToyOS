//! The GPU device's messages (§5.7.6): their codes, and each structure as the
//! little-endian words it is on the wire.
//!
//! **Every structure here is a whole number of `le32` words**, a `le64` as
//! its low word then its high one, so a message is written into the grant and
//! read out of it as words.

/// §5.7.2: the queues, by index.
pub const CONTROL_QUEUE: u16 = 0;
pub const CURSOR_QUEUE: u16 = 1;

/// §5.7.3: the one feature bit this driver takes.
pub const F_EDID: u64 = 1 << 1;

/// §5.7.4: the configuration's fields, by offset, and the one event.
pub const EVENTS_READ: usize = 0;
pub const EVENTS_CLEAR: usize = 4;
pub const NUM_SCANOUTS: usize = 8;
pub const EVENT_DISPLAY: u32 = 1 << 0;

/// §5.7.6.7: the answers this driver reads.
pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const RESP_OK_EDID: u32 = 0x1104;
pub const RESP_ERR_UNSPEC: u32 = 0x1200;
pub const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
pub const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
pub const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
pub const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

/// §5.7.6.7: `VIRTIO_GPU_FLAG_FENCE`.
pub const FLAG_FENCE: u32 = 1 << 0;

/// §5.7.6.8: the two formats this driver creates resources in, whose bytes
/// in memory are blue, green, red, then alpha or nothing.
pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;
pub const FORMAT_B8G8R8X8_UNORM: u32 = 2;

/// §5.7.6.6: a cursor's resource is 64x64.
pub const CURSOR_SIDE: u32 = 64;

/// Bytes of each structure: §5.7.6.7's header, and the answers with data
/// (§5.7.6.8): sixteen scanouts' `virtio_gpu_display_one`, and the EDID's
/// `size`, `padding` and 1024 bytes.
pub const HDR_BYTES: u32 = 24;
pub const DISPLAY_INFO_BYTES: u32 = HDR_BYTES + 16 * 24;
pub const EDID_BYTES: u32 = HDR_BYTES + 8 + 1024;
/// Where the EDID's own bytes start in its answer.
pub const EDID_AT: usize = HDR_BYTES as usize + 8;

/// A command this driver sends, by what it is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    GetDisplayInfo,
    ResourceCreate2d,
    ResourceUnref,
    SetScanout,
    ResourceFlush,
    TransferToHost2d,
    ResourceAttachBacking,
    GetEdid,
    UpdateCursor,
    MoveCursor,
}

impl Command {
    /// §5.7.6.7's `virtio_gpu_ctrl_type`.
    pub fn code(self) -> u32 {
        match self {
            Self::GetDisplayInfo => 0x0100,
            Self::ResourceCreate2d => 0x0101,
            Self::ResourceUnref => 0x0102,
            Self::SetScanout => 0x0103,
            Self::ResourceFlush => 0x0104,
            Self::TransferToHost2d => 0x0105,
            Self::ResourceAttachBacking => 0x0106,
            Self::GetEdid => 0x010a,
            Self::UpdateCursor => 0x0300,
            Self::MoveCursor => 0x0301,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::GetDisplayInfo => "GET_DISPLAY_INFO",
            Self::ResourceCreate2d => "RESOURCE_CREATE_2D",
            Self::ResourceUnref => "RESOURCE_UNREF",
            Self::SetScanout => "SET_SCANOUT",
            Self::ResourceFlush => "RESOURCE_FLUSH",
            Self::TransferToHost2d => "TRANSFER_TO_HOST_2D",
            Self::ResourceAttachBacking => "RESOURCE_ATTACH_BACKING",
            Self::GetEdid => "GET_EDID",
            Self::UpdateCursor => "UPDATE_CURSOR",
            Self::MoveCursor => "MOVE_CURSOR",
        }
    }

    /// The answer it succeeds with, and how many bytes that answer is.
    pub fn ok(self) -> (u32, u32) {
        match self {
            Self::GetDisplayInfo => (RESP_OK_DISPLAY_INFO, DISPLAY_INFO_BYTES),
            Self::GetEdid => (RESP_OK_EDID, EDID_BYTES),
            _ => (RESP_OK_NODATA, HDR_BYTES),
        }
    }
}

/// An answer's name, where §5.7.6.7 gives it one.
pub fn answer_name(code: u32) -> Option<&'static str> {
    Some(match code {
        RESP_OK_NODATA => "RESP_OK_NODATA",
        RESP_OK_DISPLAY_INFO => "RESP_OK_DISPLAY_INFO",
        RESP_OK_EDID => "RESP_OK_EDID",
        RESP_ERR_UNSPEC => "RESP_ERR_UNSPEC",
        RESP_ERR_OUT_OF_MEMORY => "RESP_ERR_OUT_OF_MEMORY",
        RESP_ERR_INVALID_SCANOUT_ID => "RESP_ERR_INVALID_SCANOUT_ID",
        RESP_ERR_INVALID_RESOURCE_ID => "RESP_ERR_INVALID_RESOURCE_ID",
        RESP_ERR_INVALID_CONTEXT_ID => "RESP_ERR_INVALID_CONTEXT_ID",
        RESP_ERR_INVALID_PARAMETER => "RESP_ERR_INVALID_PARAMETER",
        _ => return None,
    })
}

/// §5.7.6.8's `virtio_gpu_rect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn whole(width: u32, height: u32) -> Self {
        Self { x: 0, y: 0, width, height }
    }

    pub fn words(self) -> [u32; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

/// §5.7.6.7's header: `type`, `flags`, `fence_id`, `ctx_id`, then `ring_idx`
/// and its padding, which this driver leaves 0.
pub fn header(command: Command, flags: u32, fence: u64) -> [u32; 6] {
    [command.code(), flags, fence as u32, (fence >> 32) as u32, 0, 0]
}
