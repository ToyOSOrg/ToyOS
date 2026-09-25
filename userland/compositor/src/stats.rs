use window::Traffic;

/// Counters for one reporting window, flushed from a composited frame and
/// never otherwise: a compositor with nothing to draw says nothing, as soundd
/// says nothing with no clients.
///
/// Here to be read off `/log/kernel.log` on a machine whose only other
/// instrument is the panel. `damage_px_max` is the one to read first: it is the
/// largest single frame any interval contained, so it says whether one typed
/// character, one clock tick or one dragged window still costs a repaint of
/// something much larger than itself. `damage_px` over `frames` is the average
/// of the same question.
///
/// There is no scanout *read* figure because there is nothing that could
/// produce one: the panel is held as a `window::Screen`, which returns no pixel
/// and hands out no pointer. `back_rd_bytes` is where the reads went instead —
/// the cursor's blend and `fill_rect`'s row replication, in system RAM.
#[derive(Default)]
pub struct FrameStats {
    pub frames: u32,
    pub cursor_draws: u32,
    rects: u32,
    damage_px: u64,
    damage_px_max: u64,
    composite_ns_min: u64,
    composite_ns_max: u64,
    composite_ns_total: u64,
}

impl FrameStats {
    /// `composite_ns` covers composing every region of the frame, the software
    /// cursor and the blits that carry them to the panel — everything between
    /// one frame's damage being taken and it being on screen. Not the
    /// `gpu::present` calls that follow: those are syscalls, and on the
    /// firmware framebuffer they do nothing at all.
    pub fn record(&mut self, composite_ns: u64, rects: usize, damage_px: u64) {
        self.composite_ns_min = if self.frames == 0 {
            composite_ns
        } else {
            self.composite_ns_min.min(composite_ns)
        };
        self.composite_ns_max = self.composite_ns_max.max(composite_ns);
        self.composite_ns_total += composite_ns;
        self.rects += rects as u32;
        self.damage_px += damage_px;
        self.damage_px_max = self.damage_px_max.max(damage_px);
        self.frames += 1;
    }

    /// `moved` is the panel traffic of this window alone and `composed` the
    /// back buffer's. Totals rather than means: with `frames` beside them the
    /// mean is a division, and the total is the share of the window that
    /// compositing cost, which the mean is not.
    pub fn report(&self, moved: (u64, u64), composed: Traffic, windows: usize) {
        eprintln!(
            "compositor: frames={} rects={} damage_px={} damage_px_max={} \
             composite_us_min={} composite_us_max={} composite_us_total={} \
             scanout_wr_bytes={} scanout_blits={} back_rd_bytes={} cursor={} windows={}",
            self.frames,
            self.rects,
            self.damage_px,
            self.damage_px_max,
            self.composite_ns_min / 1_000,
            self.composite_ns_max / 1_000,
            self.composite_ns_total / 1_000,
            moved.0,
            moved.1,
            composed.read,
            self.cursor_draws,
            windows,
        );
    }
}

/// Every reported window summed, for `inspect`: the console says a window at a
/// time and this is what they add up to. No new measurement — the same four
/// counters [`FrameStats::record`] fills.
#[derive(Default, Clone, Copy)]
pub struct FrameTotals {
    frames: u64,
    rects: u64,
    damage_px: u64,
    composite_ns: u64,
}

impl FrameTotals {
    /// Add a window the console has just reported, before it is reset.
    pub fn fold(&mut self, window: &FrameStats) {
        *self = self.plus(window);
    }

    /// These totals with the still-open window on top: where the compositor is
    /// now.
    pub fn plus(self, window: &FrameStats) -> Self {
        Self {
            frames: self.frames + u64::from(window.frames),
            rects: self.rects + u64::from(window.rects),
            damage_px: self.damage_px + window.damage_px,
            composite_ns: self.composite_ns + window.composite_ns_total,
        }
    }

    pub fn inspect(&self, snap: &mut toyos_inspect::Snapshot) {
        snap.put("frames.composited", self.frames);
        snap.put("frames.rects", self.rects);
        snap.put("frames.damage_px", self.damage_px);
        snap.put("frames.composite_us", self.composite_ns / 1_000);
    }
}
