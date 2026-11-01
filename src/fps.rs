//! FPS from present events. Pure: time is QPC ticks plus the frequency.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    D3d9,
    Dxgi,
    Kernel,
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
        }
    }

    /// Counts one present event. Returns a sample when the 1 s window closes.
    pub fn on_present(
        &mut self,
        pid: u32,
        target: u32,
        src: Source,
        now: i64,
    ) -> Option<f32> {
        if pid != target {
            return None;
        }
        if target != self.target {
            self.target = target;
            self.start = now;
            self.d3d9 = 0;
            self.dxgi = 0;
            self.kernel = 0;
            self.saw_user_mode = false;
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
        (frames > 0).then(|| frames as f32 / secs)
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
    ) -> Option<f32> {
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

    #[test]
    fn dxgi_120_over_1s() {
        let mut c = FpsCounter::new(1000);
        assert!(approx(feed(&mut c, 1, 1, Source::Dxgi, 0, 1000, 120), 120.0));
    }

    #[test]
    fn d3d9_wins_over_dxgi() {
        let mut c = FpsCounter::new(1000);
        let mut last = None;
        for i in 0..180 {
            let src = if i % 3 == 0 { Source::D3d9 } else { Source::Dxgi };
            last = c.on_present(1, 1, src, 1000 * i / 179);
        }
        assert!(approx(last, 60.0), "got {last:?}");
    }

    #[test]
    fn kernel_only_below_20_dropped() {
        let mut c = FpsCounter::new(1000);
        assert_eq!(feed(&mut c, 1, 1, Source::Kernel, 0, 1000, 10), None);
    }

    #[test]
    fn kernel_only_above_20_counts() {
        let mut c = FpsCounter::new(1000);
        assert!(approx(feed(&mut c, 1, 1, Source::Kernel, 0, 1000, 30), 30.0));
    }

    #[test]
    fn other_pid_ignored() {
        let mut c = FpsCounter::new(1000);
        assert_eq!(feed(&mut c, 2, 1, Source::Dxgi, 0, 1000, 120), None);
        assert!(!c.saw_user_mode());
    }

    #[test]
    fn target_change_discards_window() {
        let mut c = FpsCounter::new(1000);
        feed(&mut c, 1, 1, Source::Dxgi, 0, 490, 50);
        assert!(approx(feed(&mut c, 2, 2, Source::Dxgi, 500, 1500, 30), 30.0));
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
}
