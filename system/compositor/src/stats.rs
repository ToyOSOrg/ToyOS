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
    /// cursor and the blits that carry them to the panel — everything between
    /// one frame's damage being taken and it being on screen. Not the
    /// `gpu::present` calls that follow: those are syscalls, and on the
    /// firmware framebuffer they do nothing at all.
    pub fn record(&mut self, composite_ns: u64, rects: usize, damage_px: u64) {
        self.frames += 1;
        self.rects += rects as u64;
        self.damage_px += damage_px;
        self.composite_ns += composite_ns;
    }

    pub fn inspect(&self, snap: &mut toyos_inspect::Snapshot) {
        snap.put("frames.composited", self.frames);
        snap.put("frames.rects", self.rects);
        snap.put("frames.damage_px", self.damage_px);
        snap.put("frames.composite_us", self.composite_ns / 1_000);
    }
}
