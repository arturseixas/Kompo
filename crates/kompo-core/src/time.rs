//! Time representation.
//!
//! All timeline positions are integer *ticks* of 1/705 600 000 s (a "flick").
//! This rate divides evenly into every common video frame rate (including the
//! NTSC 1001 rates) and every common audio sample rate, so frame and sample
//! boundaries are exact integers and never accumulate floating point drift.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// Ticks per second (flicks).
pub const TICKS_PER_SECOND: i64 = 705_600_000;

/// A rational number, used for frame rates, time bases and pixel aspect ratios.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new_raw(num: i64, den: i64) -> Self {
        Self { num, den }
    }

    /// Creates a normalized rational. Panics if `den == 0`.
    pub fn new(num: i64, den: i64) -> Self {
        assert!(den != 0, "rational with zero denominator");
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()) as i64;
        let s = if den < 0 { -1 } else { 1 };
        Self { num: s * num / g.max(1), den: s * den / g.max(1) }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn inverse(self) -> Self {
        Self::new(self.den, self.num)
    }

    pub fn is_valid(self) -> bool {
        self.den != 0 && self.num != 0
    }

    /// Best rational approximation of `x` with denominator <= `max_den`
    /// (continued fractions). Recognizes the NTSC rates exactly.
    pub fn approximate(x: f64, max_den: i64) -> Self {
        for r in FrameRate::COMMON {
            if (r.as_f64() - x).abs() < 1e-4 {
                return r;
            }
        }
        let (mut p0, mut q0, mut p1, mut q1) = (0i64, 1i64, 1i64, 0i64);
        let mut v = x;
        for _ in 0..32 {
            let a = v.floor() as i64;
            let p2 = a * p1 + p0;
            let q2 = a * q1 + q0;
            if q2 > max_den {
                break;
            }
            p0 = p1;
            q0 = q1;
            p1 = p2;
            q1 = q2;
            let f = v - a as f64;
            if f.abs() < 1e-12 {
                break;
            }
            v = 1.0 / f;
        }
        if q1 == 0 { Self::new(x.round() as i64, 1) } else { Self::new(p1, q1) }
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 { write!(f, "{}", self.num) } else { write!(f, "{}/{}", self.num, self.den) }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Frame rate helpers.
pub struct FrameRate;

impl FrameRate {
    pub const FPS_23_976: Rational = Rational::new_raw(24000, 1001);
    pub const FPS_24: Rational = Rational::new_raw(24, 1);
    pub const FPS_25: Rational = Rational::new_raw(25, 1);
    pub const FPS_29_97: Rational = Rational::new_raw(30000, 1001);
    pub const FPS_30: Rational = Rational::new_raw(30, 1);
    pub const FPS_48: Rational = Rational::new_raw(48, 1);
    pub const FPS_50: Rational = Rational::new_raw(50, 1);
    pub const FPS_59_94: Rational = Rational::new_raw(60000, 1001);
    pub const FPS_60: Rational = Rational::new_raw(60, 1);
    pub const FPS_120: Rational = Rational::new_raw(120, 1);

    pub const COMMON: [Rational; 10] = [
        Self::FPS_23_976,
        Self::FPS_24,
        Self::FPS_25,
        Self::FPS_29_97,
        Self::FPS_30,
        Self::FPS_48,
        Self::FPS_50,
        Self::FPS_59_94,
        Self::FPS_60,
        Self::FPS_120,
    ];

    /// Nominal integer rate used for timecode (30 for 29.97, ...).
    pub fn nominal(rate: Rational) -> i64 {
        (rate.as_f64()).round() as i64
    }

    pub fn is_drop_frame_capable(rate: Rational) -> bool {
        rate == Self::FPS_29_97 || rate == Self::FPS_59_94
    }
}

/// A point (or duration) in time, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const ZERO: Time = Time(0);
    pub const MAX: Time = Time(i64::MAX / 4);
    pub const MIN: Time = Time(i64::MIN / 4);
    pub const SECOND: Time = Time(TICKS_PER_SECOND);

    pub fn from_seconds(s: f64) -> Self {
        Time((s * TICKS_PER_SECOND as f64).round() as i64)
    }

    pub fn seconds(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    pub fn ticks(self) -> i64 {
        self.0
    }

    /// Duration of one frame at `rate`.
    pub fn frame_duration(rate: Rational) -> Time {
        Time::from_frames(1, rate)
    }

    /// Exact time of frame `frame` at `rate` (rounded to the nearest tick for exotic rates).
    pub fn from_frames(frame: i64, rate: Rational) -> Self {
        let n = frame as i128 * TICKS_PER_SECOND as i128 * rate.den as i128;
        let d = rate.num as i128;
        Time(div_round(n, d) as i64)
    }

    /// Index of the frame that contains this time.
    pub fn to_frames_floor(self, rate: Rational) -> i64 {
        let n = self.0 as i128 * rate.num as i128;
        let d = TICKS_PER_SECOND as i128 * rate.den as i128;
        n.div_euclid(d) as i64
    }

    /// Nearest frame index.
    pub fn to_frames_round(self, rate: Rational) -> i64 {
        let n = self.0 as i128 * rate.num as i128;
        let d = TICKS_PER_SECOND as i128 * rate.den as i128;
        div_round(n, d) as i64
    }

    /// Snaps to the start of the nearest frame boundary.
    pub fn snap_to_frame(self, rate: Rational) -> Self {
        Time::from_frames(self.to_frames_round(rate), rate)
    }

    /// Snaps down to the start of the frame containing this time.
    pub fn floor_to_frame(self, rate: Rational) -> Self {
        Time::from_frames(self.to_frames_floor(rate), rate)
    }

    pub fn from_samples(samples: i64, sample_rate: u32) -> Self {
        let n = samples as i128 * TICKS_PER_SECOND as i128;
        Time(div_round(n, sample_rate as i128) as i64)
    }

    /// Sample index containing this time.
    pub fn to_samples(self, sample_rate: u32) -> i64 {
        let n = self.0 as i128 * sample_rate as i128;
        n.div_euclid(TICKS_PER_SECOND as i128) as i64
    }

    /// Converts a timestamp in a stream time base (e.g. FFmpeg `time_base`) to ticks.
    pub fn from_timebase(ts: i64, tb: Rational) -> Self {
        let n = ts as i128 * tb.num as i128 * TICKS_PER_SECOND as i128;
        Time(div_round(n, tb.den as i128) as i64)
    }

    /// Converts ticks to a timestamp in the given time base (rounded down).
    pub fn to_timebase(self, tb: Rational) -> i64 {
        let n = self.0 as i128 * tb.den as i128;
        let d = tb.num as i128 * TICKS_PER_SECOND as i128;
        n.div_euclid(d) as i64
    }

    pub fn max(self, o: Time) -> Time {
        if self >= o { self } else { o }
    }

    pub fn min(self, o: Time) -> Time {
        if self <= o { self } else { o }
    }

    pub fn clamp(self, lo: Time, hi: Time) -> Time {
        self.max(lo).min(hi)
    }

    pub fn abs(self) -> Time {
        Time(self.0.abs())
    }

    /// Scales a duration by a floating factor (used for speed changes).
    pub fn mul_f64(self, k: f64) -> Time {
        Time((self.0 as f64 * k).round() as i64)
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }
}

fn div_round(n: i128, d: i128) -> i128 {
    if d < 0 {
        return div_round(-n, -d);
    }
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    if r * 2 >= d { q + 1 } else { q }
}

impl Add for Time {
    type Output = Time;
    fn add(self, o: Time) -> Time {
        Time(self.0 + o.0)
    }
}
impl Sub for Time {
    type Output = Time;
    fn sub(self, o: Time) -> Time {
        Time(self.0 - o.0)
    }
}
impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}
impl AddAssign for Time {
    fn add_assign(&mut self, o: Time) {
        self.0 += o.0;
    }
}
impl SubAssign for Time {
    fn sub_assign(&mut self, o: Time) {
        self.0 -= o.0;
    }
}
impl std::ops::Mul<i64> for Time {
    type Output = Time;
    fn mul(self, k: i64) -> Time {
        Time(self.0 * k)
    }
}
impl std::iter::Sum for Time {
    fn sum<I: Iterator<Item = Time>>(iter: I) -> Time {
        iter.fold(Time::ZERO, |a, b| a + b)
    }
}

/// Half-open interval `[start, start + duration)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Time,
    pub duration: Time,
}

impl TimeRange {
    pub fn new(start: Time, duration: Time) -> Self {
        Self { start, duration }
    }

    pub fn from_bounds(start: Time, end: Time) -> Self {
        Self { start, duration: end - start }
    }

    pub fn end(&self) -> Time {
        self.start + self.duration
    }

    pub fn contains(&self, t: Time) -> bool {
        t >= self.start && t < self.end()
    }

    pub fn overlaps(&self, o: &TimeRange) -> bool {
        self.start < o.end() && o.start < self.end()
    }

    pub fn intersection(&self, o: &TimeRange) -> Option<TimeRange> {
        let s = self.start.max(o.start);
        let e = self.end().min(o.end());
        (e > s).then(|| TimeRange::from_bounds(s, e))
    }

    pub fn is_empty(&self) -> bool {
        self.duration.0 <= 0
    }
}

/// SMPTE timecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub frames: i64,
    pub drop_frame: bool,
    pub negative: bool,
}

impl Timecode {
    /// Builds a timecode from a frame count. Drop-frame numbering is used only
    /// when `drop_frame` is set and the rate is 29.97 or 59.94.
    pub fn from_frames(frame: i64, rate: Rational, drop_frame: bool) -> Self {
        let negative = frame < 0;
        let mut f = frame.abs();
        let nominal = FrameRate::nominal(rate).max(1);
        let df = drop_frame && FrameRate::is_drop_frame_capable(rate);
        if df {
            let drop = if nominal == 30 { 2 } else { 4 };
            let frames_per_10min = nominal * 600 - drop * 9;
            let frames_per_min = nominal * 60 - drop;
            let d = f / frames_per_10min;
            let m = f % frames_per_10min;
            if m > drop {
                f += drop * 9 * d + drop * ((m - drop) / frames_per_min);
            } else {
                f += drop * 9 * d;
            }
        }
        let frames = f % nominal;
        let total_secs = f / nominal;
        Timecode { hours: total_secs / 3600, minutes: (total_secs / 60) % 60, seconds: total_secs % 60, frames, drop_frame: df, negative }
    }

    pub fn from_time(t: Time, rate: Rational, drop_frame: bool) -> Self {
        Self::from_frames(t.to_frames_floor(rate), rate, drop_frame)
    }

    pub fn to_frames(&self, rate: Rational) -> i64 {
        let nominal = FrameRate::nominal(rate).max(1);
        let total_minutes = self.hours * 60 + self.minutes;
        let mut f = ((self.hours * 3600 + self.minutes * 60 + self.seconds) * nominal) + self.frames;
        if self.drop_frame && FrameRate::is_drop_frame_capable(rate) {
            let drop = if nominal == 30 { 2 } else { 4 };
            f -= drop * (total_minutes - total_minutes / 10);
        }
        if self.negative { -f } else { f }
    }

    /// Parses `HH:MM:SS:FF` or `HH:MM:SS;FF` (drop frame).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (negative, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let drop_frame = s.contains(';');
        let parts: Vec<&str> = s.split([':', ';', '.']).collect();
        if parts.len() != 4 {
            return None;
        }
        let n: Vec<i64> = parts.iter().map(|p| p.parse().ok()).collect::<Option<_>>()?;
        Some(Timecode { hours: n[0], minutes: n[1], seconds: n[2], frames: n[3], drop_frame, negative })
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{}{:02}:{:02}:{:02}{}{:02}",
            if self.negative { "-" } else { "" },
            self.hours,
            self.minutes,
            self.seconds,
            sep,
            self.frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_exact_for_common_rates() {
        for r in FrameRate::COMMON {
            for f in [0i64, 1, 2, 999, 1_000_000] {
                let t = Time::from_frames(f, r);
                assert_eq!(t.to_frames_floor(r), f, "rate {r}");
                assert_eq!(t.to_frames_round(r), f);
            }
            // tick count is exact (no rounding needed) for every common rate
            let n = TICKS_PER_SECOND as i128 * r.den as i128;
            assert_eq!(n % r.num as i128, 0, "rate {r} not exact");
        }
    }

    #[test]
    fn samples_are_exact() {
        for sr in [44_100u32, 48_000, 88_200, 96_000, 192_000] {
            assert_eq!(TICKS_PER_SECOND % sr as i64, 0);
            assert_eq!(Time::from_samples(12345, sr).to_samples(sr), 12345);
        }
    }

    #[test]
    fn timebase_roundtrip() {
        let tb = Rational::new(1, 90_000);
        let t = Time::from_timebase(3003, tb);
        assert_eq!(t, Time::from_frames(1, FrameRate::FPS_29_97));
        assert_eq!(t.to_timebase(tb), 3003);
    }

    #[test]
    fn drop_frame_timecode() {
        let r = FrameRate::FPS_29_97;
        let tc = Timecode::from_frames(1800, r, true);
        assert_eq!(tc.to_string(), "00:01:00;02");
        assert_eq!(tc.to_frames(r), 1800);
        let tc = Timecode::from_frames(17982, r, true);
        assert_eq!(tc.to_string(), "00:10:00;00");
        for f in [0, 1, 1799, 1800, 1801, 17981, 17982, 107_892, 2_589_407] {
            let tc = Timecode::from_frames(f, r, true);
            assert_eq!(tc.to_frames(r), f, "{tc}");
            assert_eq!(Timecode::parse(&tc.to_string()).unwrap().to_frames(r), f);
        }
    }

    #[test]
    fn non_drop_timecode() {
        let r = FrameRate::FPS_25;
        let tc = Timecode::from_frames(90_000 + 12, r, false);
        assert_eq!(tc.to_string(), "01:00:00:12");
        assert_eq!(tc.to_frames(r), 90_012);
    }

    #[test]
    fn approximate_rates() {
        assert_eq!(Rational::approximate(29.97002997, 100_000), FrameRate::FPS_29_97);
        assert_eq!(Rational::approximate(25.0, 100_000), Rational::new(25, 1));
        assert_eq!(Rational::approximate(12.5, 1000), Rational::new(25, 2));
    }

    #[test]
    fn ranges() {
        let a = TimeRange::new(Time(10), Time(10));
        let b = TimeRange::new(Time(15), Time(10));
        assert!(a.overlaps(&b));
        assert_eq!(a.intersection(&b), Some(TimeRange::new(Time(15), Time(5))));
        assert!(!a.overlaps(&TimeRange::new(Time(20), Time(1))));
    }
}
