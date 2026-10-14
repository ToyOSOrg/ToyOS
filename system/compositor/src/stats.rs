/// Every frame composited so far, for `inspect`, which is where they are read.
#[derive(Default)]
pub struct FrameTotals {
    frames: u64,
    rects: u64,
    damage_px: u64,
    composite_ns: u64,
}

impl FrameTotals {
    /// `composite_ns` covers composing every region of the frame, the software
    /// cursor and the blits that carry them to the scanout. Not the display's
    /// present that follows: virtio-gpu's transfers and flushes, and nothing
    /// at all on the firmware framebuffer.
    pub fn record(&mut self, composite_ns: u64, rects: usize, damage_px: u64) {
        self.frames += 1;
        self.rects += rects as u64;
        self.damage_px += damage_px;
        self.composite_ns += composite_ns;
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn inspect(&self, snap: &mut toyos_inspect::Snapshot) {
        snap.put("frames.composited", self.frames);
        snap.put("frames.rects", self.rects);
        snap.put("frames.damage_px", self.damage_px);
        snap.put("frames.composite_us", self.composite_ns / 1_000);
    }
}
