//! FPS from present events. Pure: time is QPC ticks plus the frequency.
//! Also keeps the last 10 s of present gaps for the 1% low.

/// Gaps kept for the 1% low. Same source, so a monotonic ring is enough.
/// ponytail: above ~400 fps this covers < 10 s; raise it if that ever matters.
pub const LOW_CAP: usize = 4096;

/// One closed 1 s window.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sample {
    /// Frames per second over the window, as always.
    pub fps: f32,
    /// Nearest-rank p99 frame time of the last 10 s, in FPS. `None` under 100 frames.
    pub low: Option<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    D3d9,
    Dxgi,
    Kernel,
}

impl Source {
    /// Counting and frame times both use the highest-priority source only.
    fn rank(self) -> u8 {
        match self {
            Source::D3d9 => 2,
            Source::Dxgi => 1,
            Source::Kernel => 0,
        }
    }
}

/// Per-target 1 s present accumulator. Lives on the ETW consumer thread only.
pub struct FpsCounter {
    freq: i64,
    target: u32,
    start: i64,
    d3d9: u32,
    dxgi: u32,
    kernel: u32,
    saw_user_mode: bool,
    /// Ring of present gaps in QPC ticks, newest at `head - 1`. Only gaps that fit
    /// a `u32` are recorded; above a ~2.1 GHz clock that drops the longest frames.
    gaps: Box<[u32]>,
    /// Selection scratch, so the 1% low never allocates in the callback.
    scratch: Box<[u32]>,
    head: usize,
    len: usize,
    last: i64,
    low_src: Option<Source>,
}

impl FpsCounter {
    pub fn new(qpc_freq: i64) -> Self {
        Self {
            freq: qpc_freq.max(1),
            target: 0,
            start: 0,
            d3d9: 0,
            dxgi: 0,
            kernel: 0,
            saw_user_mode: false,
            gaps: vec![0; LOW_CAP].into_boxed_slice(),
            scratch: vec![0; LOW_CAP].into_boxed_slice(),
            head: 0,
            len: 0,
            last: 0,
            low_src: None,
        }
    }

    /// Counts one present event. Returns a sample when the 1 s window closes.
    pub fn on_present(&mut self, pid: u32, target: u32, src: Source, now: i64) -> Option<Sample> {
        if pid != target {
            return None;
        }
        if target != self.target {
            self.reset(target, now);
        }
        // Frame times use one source only: the highest priority seen since the reset.
        // A better source takes over with empty history. A worse one is ignored until
        // the current one has been silent for 2 s, then it takes over with empty
        // history too: a splash-screen source must not pin the 1% low at `---`.
        match self.low_src {
            Some(cur) if src.rank() == cur.rank() => {
                let gap = now - self.last;
                self.last = now;
                // Pauses, stalls and clock jumps are not frame times, and a gap
                // that does not fit a `u32` is no frame time either.
                if gap > 0
                    && gap <= 2 * self.freq
                    && let Ok(gap) = u32::try_from(gap)
                {
                    self.gaps[self.head] = gap;
                    self.head = (self.head + 1) % LOW_CAP;
                    self.len = (self.len + 1).min(LOW_CAP);
                }
            }
            Some(cur) if src.rank() < cur.rank() && now - self.last <= 2 * self.freq => {}
            _ => {
                self.low_src = Some(src);
                self.head = 0;
                self.len = 0;
                self.last = now;
            }
        }
        match src {
            Source::D3d9 => {
                self.d3d9 += 1;
                self.saw_user_mode = true;
            }
            Source::Dxgi => {
                self.dxgi += 1;
                self.saw_user_mode = true;
            }
            Source::Kernel => self.kernel += 1,
        }
        let ticks = now - self.start;
        if ticks < self.freq {
            return None;
        }
        let secs = ticks as f32 / self.freq as f32;
        let stale = ticks > 2 * self.freq;
        // ponytail: DxgKrnl is kernel-mode so it cannot be PID-filtered at source;
        // the >= 20 floor is what keeps desktop apps out.
        let frames = if self.d3d9 > 0 {
            self.d3d9
        } else if self.dxgi > 0 {
            self.dxgi
        } else if self.kernel as f32 >= 20.0 * secs {
            self.kernel
        } else {
            0
        };
        self.d3d9 = 0;
        self.dxgi = 0;
        self.kernel = 0;
        self.start = now;
        // A game that was paused/stalled would otherwise report a bogus low rate.
        if stale || frames == 0 {
            return None;
        }
        Some(Sample {
            fps: frames as f32 / secs,
            low: self.low(),
        })
    }

    /// Nearest-rank p99 frame time over the last 10 s of gaps, as FPS.
    /// Runs once per closed window: at most `LOW_CAP` walks and a selection.
    fn low(&mut self) -> Option<f32> {
        let window = (10 * self.freq) as u64;
        let mut n = 0;
        let mut sum = 0u64;
        while n < self.len {
            let gap = u64::from(self.gaps[(self.head + LOW_CAP - 1 - n) % LOW_CAP]);
            if sum + gap > window {
                break;
            }
            sum += gap;
            n += 1;
        }
        if n < 100 {
            return None;
        }
        for (k, slot) in self.scratch[..n].iter_mut().enumerate() {
            *slot = self.gaps[(self.head + LOW_CAP - 1 - k) % LOW_CAP];
        }
        let idx = (99 * n).div_ceil(100) - 1;
        let (_, gap, _) = self.scratch[..n].select_nth_unstable(idx);
        Some(self.freq as f32 / *gap as f32)
    }

    /// Discards any partial window and re-anchors on `target` at `now`.
    pub fn reset(&mut self, target: u32, now: i64) {
        self.target = target;
        self.start = now;
        self.d3d9 = 0;
        self.dxgi = 0;
        self.kernel = 0;
        self.saw_user_mode = false;
        self.head = 0;
        self.len = 0;
        self.low_src = None;
    }

    /// Any D3d9/Dxgi event for the current target since the last reset.
    pub fn saw_user_mode(&self) -> bool {
        self.saw_user_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `n` events spread evenly over [from, to]; returns the last sample.
    fn feed(
        c: &mut FpsCounter,
        pid: u32,
        target: u32,
        src: Source,
        from: i64,
        to: i64,
        n: i64,
    ) -> Option<Sample> {
        let mut last = None;
        for i in 0..n {
            let t = from + (to - from) * i / (n - 1).max(1);
            last = c.on_present(pid, target, src, t);
        }
        last
    }

    fn approx(v: Option<f32>, want: f32) -> bool {
        v.is_some_and(|f| (f - want).abs() < 0.5)
    }

    fn low_approx(s: Option<Sample>, want: f32) -> bool {
        s.and_then(|s| s.low)
            .is_some_and(|f| (f - want).abs() < 0.5)
    }

    #[test]
    fn dxgi_120_over_1s() {
        let mut c = FpsCounter::new(1000);
        assert!(approx(
            feed(&mut c, 1, 1, Source::Dxgi, 0, 1000, 120).map(|s| s.fps),
            120.0
        ));
    }

    #[test]
    fn d3d9_wins_over_dxgi() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..180 {
            let src = if i % 3 == 0 {
                Source::D3d9
            } else {
                Source::Dxgi
            };
            last = c.on_present(1, 1, src, 1000 * i / 179);
        }
        assert!(approx(last.map(|s| s.fps), 60.0), "got {last:?}");
    }

    #[test]
    fn kernel_only_below_20_dropped() {
        let mut c = FpsCounter::new(1000);
        assert_eq!(
            feed(&mut c, 1, 1, Source::Kernel, 0, 1000, 10).map(|s| s.fps),
            None
        );
    }

    #[test]
    fn kernel_only_above_20_counts() {
        let mut c = FpsCounter::new(1000);
        assert!(approx(
            feed(&mut c, 1, 1, Source::Kernel, 0, 1000, 30).map(|s| s.fps),
            30.0
        ));
    }

    #[test]
    fn other_pid_ignored() {
        let mut c = FpsCounter::new(1000);
        assert_eq!(
            feed(&mut c, 2, 1, Source::Dxgi, 0, 1000, 120).map(|s| s.fps),
            None
        );
        assert!(!c.saw_user_mode());
    }

    #[test]
    fn target_change_discards_window() {
        let mut c = FpsCounter::new(1000);
        feed(&mut c, 1, 1, Source::Dxgi, 0, 490, 50);
        assert!(approx(
            feed(&mut c, 2, 2, Source::Dxgi, 500, 1500, 30).map(|s| s.fps),
            30.0
        ));
    }

    #[test]
    fn idle_gap_discards_window() {
        let mut c = FpsCounter::new(1000);
        feed(&mut c, 1, 1, Source::Dxgi, 0, 500, 20);
        // Paused for 4.5 s: the overdue window is dropped, not reported.
        assert_eq!(c.on_present(1, 1, Source::Dxgi, 5000).map(|s| s.fps), None);
        assert!(approx(
            feed(&mut c, 1, 1, Source::Dxgi, 5100, 6000, 60).map(|s| s.fps),
            60.0
        ));
    }

    #[test]
    fn reset_drops_old_window() {
        let mut c = FpsCounter::new(1000);
        feed(&mut c, 1, 1, Source::Dxgi, 0, 900, 10);
        c.reset(2, 950); // target B, no B events
        assert_eq!(c.on_present(1, 1, Source::Dxgi, 1200).map(|s| s.fps), None); // pid 1 is not B
        c.reset(1, 1300); // back to A
        // A's old events must not close the new window.
        assert_eq!(c.on_present(1, 1, Source::Dxgi, 1400).map(|s| s.fps), None);
        assert!(approx(
            c.on_present(1, 1, Source::Dxgi, 2300).map(|s| s.fps),
            2.0
        ));
    }

    #[test]
    fn saw_user_mode_flag() {
        let mut c = FpsCounter::new(1000);
        feed(&mut c, 1, 1, Source::Kernel, 0, 100, 5);
        assert!(!c.saw_user_mode());
        c.on_present(1, 1, Source::Dxgi, 110);
        assert!(c.saw_user_mode());
        c.on_present(3, 3, Source::Kernel, 120);
        assert!(!c.saw_user_mode());
    }

    #[test]
    fn low_none_below_100_frames() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..60 {
            last = c.on_present(1, 1, Source::Dxgi, 1000 * i / 59);
        }
        assert!(approx(last.map(|s| s.fps), 60.0), "got {last:?}");
        assert_eq!(last.and_then(|s| s.low), None);
    }

    #[test]
    fn low_steady_100fps() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..=200 {
            last = c.on_present(1, 1, Source::Dxgi, 10 * i);
        }
        assert!(low_approx(last, 100.0), "got {last:?}");
    }

    #[test]
    fn low_is_p99_gap() {
        // 301 frames: 10 ms gaps, the last `spikes` gaps 50 ms, then 10 ms frames
        // up to 4010 ms so the closing sample holds every gap.
        let run = |spikes: i64| {
            let mut c = FpsCounter::new(1000);
            let mut t = 0;
            let mut last = None;
            for i in 0..401 {
                if i > 0 {
                    t += if i > 300 - spikes && i <= 300 { 50 } else { 10 };
                }
                if let Some(s) = c.on_present(1, 1, Source::Dxgi, t) {
                    last = Some(s);
                }
            }
            last
        };
        assert!(low_approx(run(4), 20.0), "got {:?}", run(4));
        assert!(low_approx(run(3), 100.0), "got {:?}", run(3));
    }

    #[test]
    fn low_window_10s() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..=100 {
            last = c.on_present(1, 1, Source::Dxgi, 50 * i);
        }
        // 5 s at 20 fps, then 11 s at 100 fps: the slow start is off the window.
        for i in 1..=1100 {
            last = c.on_present(1, 1, Source::Dxgi, 5000 + 10 * i);
        }
        assert!(low_approx(last, 100.0), "got {last:?}");
    }

    #[test]
    fn low_source_priority() {
        // Kernel gaps interleaved with DXGI must not halve DXGI's frame times.
        let mut c = FpsCounter::new(1000);
        for t in (0..2000).step_by(5) {
            let _ = c.on_present(1, 1, Source::Kernel, t);
            if t % 10 == 0 {
                let _ = c.on_present(1, 1, Source::Dxgi, t);
            }
        }
        let last = c.on_present(1, 1, Source::Dxgi, 2000);
        assert!(low_approx(last, 100.0), "got {last:?}");

        // Kernel first (Vulkan/GL), then DXGI appears mid-window and takes over.
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..=200 {
            last = c.on_present(1, 1, Source::Kernel, 5 * i);
        }
        assert!(low_approx(last, 200.0), "got {last:?}");
        for i in 0..=150 {
            let s = c.on_present(1, 1, Source::Dxgi, 1500 + 10 * i);
            if i == 50 {
                assert_eq!(s.and_then(|s| s.low), None, "kernel gaps survived");
            }
            last = s;
        }
        assert!(low_approx(last, 100.0), "got {last:?}");
    }

    #[test]
    fn low_source_falls_back_after_2s() {
        // A splash screen in D3D9 (10 ms) must not pin the 1% low once it goes
        // silent: DXGI takes over after 2 s and only its own 5 ms gaps count.
        let mut c = FpsCounter::new(1000);
        let mut t = 0;
        let mut last = None;
        for _ in 0..150 {
            last = c.on_present(1, 1, Source::D3d9, t).or(last);
            t += 10;
        }
        // D3D9 stops; DXGI alone for 4 s covers the handover plus 100 gaps.
        while t <= 5500 {
            last = c.on_present(1, 1, Source::Dxgi, t).or(last);
            t += 5;
        }
        assert!(low_approx(last, 200.0), "got {last:?}");
    }

    #[test]
    fn low_ring_wraps() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..=6000 {
            last = c.on_present(1, 1, Source::Dxgi, i);
        }
        assert!(low_approx(last, 1000.0), "got {last:?}");
        // 3 ms frames evict the 1 ms ones, so the ring ends up spanning > 10 s:
        // the window must be the newest gaps, not the oldest.
        let mut t = 6000;
        for _ in 0..4100 {
            t += 3;
            last = c.on_present(1, 1, Source::Dxgi, t).or(last);
        }
        assert!(low_approx(last, 333.3), "got {last:?}");
        // 5 ms from here on, enough for a window to close.
        for _ in 0..200 {
            t += 5;
            last = c.on_present(1, 1, Source::Dxgi, t).or(last);
        }
        assert!(low_approx(last, 200.0), "got {last:?}");
    }

    #[test]
    fn low_ignores_pause_gap() {
        // Two 3 s alt-tabs among ~140 gaps (the trailing pause is inert). Nearest
        // rank leaves only 1% slack, so if the pauses were recorded p99 would be a
        // pause (0.3 fps): the filter is what keeps them out.
        let mut c = FpsCounter::new(1000);
        let mut t = 0;
        let mut last = None;
        for run in [20, 20, 101] {
            for _ in 0..run {
                last = c.on_present(1, 1, Source::Dxgi, t).or(last);
                t += 10;
            }
            t += 3000;
        }
        assert!(low_approx(last, 100.0), "got {last:?}");
    }

    #[test]
    fn reset_clears_low() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..=200 {
            last = c.on_present(1, 1, Source::Dxgi, 10 * i);
        }
        assert!(low_approx(last, 100.0), "got {last:?}");
        c.reset(1, 2000);
        for i in 1..=50 {
            last = c.on_present(1, 1, Source::Dxgi, 2000 + 20 * i);
        }
        assert_eq!(last.and_then(|s| s.low), None, "old gaps survived reset");
        assert!(approx(last.map(|s| s.fps), 50.0), "got {last:?}");
    }
}
