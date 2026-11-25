//! The request window. CLAUDE.md §6.1.
//!
//! > Treat outstanding requests as a congestion window. Start at 16 × 256 KB.
//! > AIMD: window of ACKs with no stall → `w += 1`; timeout or buffer-full →
//! > `w /= 2`. Clamp `[4, 128]`. Target `window_bytes ≈ measured_bandwidth ×
//! > RTT × 1.3` (bandwidth-delay product).
//!
//! The BDP target is a **ceiling on growth**, not a setpoint. Its job is to stop
//! the window growing past what the link can carry: every request beyond the
//! BDP only sits in the SSH connection's queue, and that queue is shared with
//! any interactive shell on the same transport (§8.1). A 32 MiB backlog in front
//! of a keystroke is a latency bug, even on a fast link.
//!
//! **Slow start** comes first, as in TCP: the window grows by one per ack
//! (doubling per round trip) until the first stall, then AIMD takes over.
//! §6.1's +1 per window alone took ~18 round trips to get from 16 to 34, and a
//! 128 MiB download at 40 ms is only ~30 round trips long: it reached 110 MB/s
//! where a deep fixed window reached 219. The BDP ceiling still bounds slow
//! start, so it cannot overshoot what the path carries.
//!
//! A "stall" is a response far slower than the path's best: more than 4× the
//! minimum RTT plus 50 ms. That is the queue building, which is exactly when to
//! back off. Halving is rate-limited to once per window of acks, as TCP does,
//! so one slow burst does not collapse the window to the floor.

use std::time::{Duration, Instant};

pub const MIN: u32 = 4;
pub const MAX: u32 = 128;
pub const START: u32 = 16;

/// BDP headroom from §6.1.
const BDP_FACTOR: f64 = 1.3;
/// EWMA weight for each new bandwidth sample.
const BW_ALPHA: f64 = 0.3;
/// Bandwidth is sampled over at least this long, so one burst is not "the rate".
const BW_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct Window {
    w: u32,
    min: u32,
    max: u32,
    /// Acks since the last increase.
    acks: u32,
    /// Doubling per round trip until the first stall.
    slow_start: bool,
    /// Acks to wait after a halving before another may happen.
    cooldown: u32,
    /// Bytes per request, for the BDP arithmetic.
    chunk: u64,
    min_rtt: Option<Duration>,
    /// Bytes per second, EWMA.
    bandwidth: Option<f64>,
    sample_start: Option<Instant>,
    sample_bytes: u64,
    /// Ceiling from the BDP; `max` until there is a measurement.
    ceiling: u32,
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub increases: u32,
    pub halvings: u32,
    pub peak: u32,
}

impl Window {
    pub fn new(chunk: u64) -> Self {
        Self::bounded(chunk, START, MIN, MAX)
    }

    /// Fixed or narrowed bounds. `bounded(c, 1, 1, 1)` is one outstanding
    /// request at a time: WinSCP's behaviour, kept for the benchmark baseline.
    pub fn bounded(chunk: u64, start: u32, min: u32, max: u32) -> Self {
        let min = min.max(1);
        let max = max.max(min);
        let w = start.clamp(min, max);
        Window {
            w,
            min,
            max,
            acks: 0,
            // A fixed window (min == max) has nothing to search for.
            slow_start: min < max,
            cooldown: 0,
            chunk: chunk.max(1),
            min_rtt: None,
            bandwidth: None,
            sample_start: None,
            sample_bytes: 0,
            ceiling: max,
            stats: Stats {
                peak: w,
                ..Default::default()
            },
        }
    }

    /// Requests that may be outstanding right now.
    pub fn get(&self) -> u32 {
        self.w
    }

    pub fn min_rtt(&self) -> Option<Duration> {
        self.min_rtt
    }

    pub fn bandwidth(&self) -> Option<f64> {
        self.bandwidth
    }

    /// A response arrived after `latency`, carrying `bytes`.
    pub fn on_ack(&mut self, latency: Duration, bytes: u64, now: Instant) {
        self.min_rtt = Some(self.min_rtt.map_or(latency, |m| m.min(latency)));
        self.sample(bytes, now);

        if self.is_stall(latency) {
            self.on_stall();
            return;
        }
        self.cooldown = self.cooldown.saturating_sub(1);
        if self.slow_start {
            if self.w < self.max.min(self.ceiling) {
                self.w += 1;
                self.stats.increases += 1;
                self.stats.peak = self.stats.peak.max(self.w);
            }
            return;
        }
        self.acks += 1;
        // "window of ACKs with no stall → w += 1"
        if self.acks >= self.w {
            self.acks = 0;
            if self.w < self.max.min(self.ceiling) {
                self.w += 1;
                self.stats.increases += 1;
                self.stats.peak = self.stats.peak.max(self.w);
            }
        }
    }

    /// "timeout or buffer-full → w /= 2". Once per window of acks.
    pub fn on_stall(&mut self) {
        if self.cooldown > 0 {
            return;
        }
        self.slow_start = false;
        let halved = (self.w / 2).max(self.min);
        if halved < self.w {
            self.w = halved;
            self.stats.halvings += 1;
        }
        self.acks = 0;
        self.cooldown = self.w;
    }

    fn is_stall(&self, latency: Duration) -> bool {
        match self.min_rtt {
            Some(rtt) => latency > rtt * 4 + Duration::from_millis(50),
            None => false,
        }
    }

    fn sample(&mut self, bytes: u64, now: Instant) {
        let start = *self.sample_start.get_or_insert(now);
        self.sample_bytes += bytes;
        let dt = now.duration_since(start);
        if dt < BW_INTERVAL {
            return;
        }
        let rate = self.sample_bytes as f64 / dt.as_secs_f64();
        self.bandwidth = Some(match self.bandwidth {
            Some(b) => b + BW_ALPHA * (rate - b),
            None => rate,
        });
        self.sample_start = Some(now);
        self.sample_bytes = 0;
        self.update_ceiling();
    }

    /// `window_bytes ≈ bandwidth × RTT × 1.3`, in requests, plus two so a
    /// window limited by itself can still grow (its measured bandwidth is its
    /// own w × chunk / RTT, so the ceiling sits 30% above it).
    fn update_ceiling(&mut self) {
        let (Some(bw), Some(rtt)) = (self.bandwidth, self.min_rtt) else {
            return;
        };
        let bdp_requests = (bw * rtt.as_secs_f64() * BDP_FACTOR / self.chunk as f64).ceil() as u32;
        self.ceiling = bdp_requests.saturating_add(2).clamp(self.min, self.max);
        // Never shrink below the ceiling by fiat; the stall rule does shrinking.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn acks(w: &mut Window, n: u32, latency: Duration, t: &mut Instant) {
        for _ in 0..n {
            *t += MS;
            w.on_ack(latency, 256 * 1024, *t);
        }
    }

    #[test]
    fn slow_start_doubles_per_window_until_the_first_stall() {
        let mut w = Window::new(256 * 1024);
        assert_eq!(w.get(), START);
        let mut t = Instant::now();
        acks(&mut w, 16, 40 * MS, &mut t);
        assert_eq!(w.get(), 32, "one window of acks doubles it");
    }

    #[test]
    fn after_a_stall_it_grows_by_one_per_window_of_acks() {
        let mut w = Window::new(256 * 1024);
        let mut t = Instant::now();
        acks(&mut w, 16, 40 * MS, &mut t); // slow start to 32
        acks(&mut w, 1, 400 * MS, &mut t); // stall: halve, AIMD from here
        assert_eq!(w.get(), 16);
        acks(&mut w, 16, 40 * MS, &mut t); // cooldown + one window
        acks(&mut w, 16, 40 * MS, &mut t);
        assert_eq!(w.get(), 17, "additive increase: +1 per window of acks");
    }

    #[test]
    fn a_stall_halves_once_then_cools_down() {
        let mut w = Window::bounded(1, 64, MIN, MAX);
        let mut t = Instant::now();
        acks(&mut w, 1, 40 * MS, &mut t); // establishes min RTT
        acks(&mut w, 1, 400 * MS, &mut t); // > 4 × 40 + 50
        assert_eq!(w.get(), 32);
        // More slow responses in the same window: no further halving.
        acks(&mut w, 5, 400 * MS, &mut t);
        assert_eq!(w.get(), 32);
    }

    #[test]
    fn never_leaves_its_bounds() {
        let mut w = Window::new(1);
        for _ in 0..20 {
            w.on_stall();
            w.cooldown = 0;
        }
        assert_eq!(w.get(), MIN);
        let mut big = Window::bounded(1, MAX, MIN, MAX);
        let mut t = Instant::now();
        acks(&mut big, 10_000, MS, &mut t);
        assert_eq!(big.get(), MAX);
    }

    #[test]
    fn the_baseline_window_is_exactly_one() {
        let mut w = Window::bounded(32 * 1024, 1, 1, 1);
        let mut t = Instant::now();
        acks(&mut w, 1000, 40 * MS, &mut t);
        assert_eq!(w.get(), 1);
    }

    /// The BDP ceiling: 10 MB/s × 40 ms × 1.3 = 520 KB, two 256 KB requests,
    /// plus two. Slow start may add a few before the first 100 ms bandwidth
    /// sample exists; once the ceiling is known, growth stops.
    #[test]
    fn growth_stops_at_the_bandwidth_delay_product() {
        let chunk = 256 * 1024u64;
        let mut w = Window::bounded(chunk, 32, MIN, MAX);
        let mut t = Instant::now();
        let mut step = |w: &mut Window| {
            // 10 MB/s: one 256 KiB response every ~26 ms, each taking 40 ms.
            t += Duration::from_micros(26_214);
            w.on_ack(40 * MS, chunk, t);
        };
        for _ in 0..10 {
            step(&mut w);
        }
        let settled = w.get();
        assert!(settled <= 32 + 5, "slow start ran past the first sample: {settled}");
        for _ in 0..200 {
            step(&mut w);
        }
        assert_eq!(w.get(), settled, "grew past the BDP ceiling");
        assert!(w.ceiling <= 6, "ceiling {}", w.ceiling);
    }
}
