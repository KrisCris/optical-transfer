//! Which pixels of each camera frame are worth decoding.
//!
//! A port of the web receiver's `RegionTracker` (`web/app/capture.js`), rule
//! for rule and constant for constant, so the two receivers schedule the same
//! work and their telemetry compares directly: one padded crop per code seen
//! recently, plus a periodic full-frame scan to acquire or reacquire codes,
//! more often while a code is missing. Pure logic over millisecond
//! timestamps, so it is tested without a camera or a clock.

/// How long a region survives without a decode or sighting. Longer TTLs were
/// measured worse upstream: a dead region squats on crop slots.
pub const REGION_TTL_MS: f64 = 1500.0;
/// Full-scan cadence: no lock, a code missing, everything locked.
pub const ACQUIRE_SCAN_MS: f64 = 100.0;
pub const DEGRADED_SCAN_MS: f64 = 250.0;
pub const LOCKED_SCAN_MS: f64 = 1500.0;
/// The "how many codes does this stream show" high-water mark ages out, so a
/// sender restarted with fewer codes stops triggering rescans.
pub const EXPECTED_DECAY_MS: f64 = 10_000.0;
pub const REGION_PAD: f32 = 0.35;
pub const MAX_REGIONS: usize = 9;
const MIN_CROP: usize = 32;

/// An axis-aligned box in capture pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Bounds {
    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A rectangle of the frame to decode, in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Acquiring,
    Degraded,
    Locked,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Acquiring => "acquiring",
            Self::Degraded => "degraded",
            Self::Locked => "locked",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Region {
    pub id: u64,
    pub bounds: Bounds,
    /// Last decode or sighting, in tracker milliseconds.
    pub seen: f64,
    /// Ever decoded. Undecoded regions come from sightings only.
    pub decoded: bool,
    /// Half-life blend of per-decode motion, in pixels.
    pub drift: f32,
    pub hits: u64,
}

/// What to decode from one frame.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub full: bool,
    pub crops: Vec<Crop>,
}

#[derive(Debug)]
pub struct RegionTracker {
    regions: Vec<Region>,
    last_full_scan: f64,
    expected: usize,
    expected_at: f64,
    rotate: usize,
    next_id: u64,
}

impl Default for RegionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RegionTracker {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            last_full_scan: f64::NEG_INFINITY,
            expected: 0,
            expected_at: 0.0,
            rotate: 0,
            next_id: 1,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// The most codes this stream has shown at once, recently.
    pub fn expected(&self) -> usize {
        self.expected
    }

    pub fn decoded_count(&self) -> usize {
        self.regions.iter().filter(|region| region.decoded).count()
    }

    /// What the scan cadence is doing.
    pub fn mode(&self) -> Mode {
        let live = self.decoded_count();
        if live == 0 {
            Mode::Acquiring
        } else if live < self.expected {
            Mode::Degraded
        } else {
            Mode::Locked
        }
    }

    /// A code decoded at `bounds`.
    pub fn note_decoded(&mut self, bounds: Bounds, now: f64) {
        if let Some(index) = self.find(bounds) {
            let region = &mut self.regions[index];
            let (x, y) = bounds.center();
            let (rx, ry) = region.bounds.center();
            // Half-life blend of per-decode motion: a steady hand decays to
            // zero, a moving one keeps the crop padding wide so it leads the
            // target.
            region.drift = 0.5 * region.drift + 0.5 * (x - rx).hypot(y - ry);
            region.bounds = bounds;
            region.seen = now;
            region.decoded = true;
            region.hits += 1;
            return;
        }
        self.add(bounds, now, true);
    }

    /// A code detected but not decoded. Keeps a region alive, never moves it:
    /// failed quads are often clipped or mis-sized, and one overwriting a
    /// decode-proven box aims every following crop at garbage.
    pub fn note_sighting(&mut self, bounds: Bounds, now: f64) {
        if let Some(index) = self.find(bounds) {
            self.regions[index].seen = now;
            return;
        }
        // A sighting may only found a region when it is sized like the codes
        // this stream already decodes; with no reference, full scans own
        // acquisition and phantom regions would only starve them.
        let Some(reference) = self.regions.iter().find(|region| region.decoded) else {
            return;
        };
        let ratio = bounds.w.max(bounds.h) / reference.bounds.w.max(reference.bounds.h);
        if !(0.5..=2.0).contains(&ratio) {
            return;
        }
        self.add(bounds, now, false);
    }

    fn find(&self, bounds: Bounds) -> Option<usize> {
        let (x, y) = bounds.center();
        self.regions.iter().position(|region| {
            let (rx, ry) = region.bounds.center();
            (x - rx).abs() < bounds.w.max(region.bounds.w) / 2.0
                && (y - ry).abs() < bounds.h.max(region.bounds.h) / 2.0
        })
    }

    fn add(&mut self, bounds: Bounds, now: f64, decoded: bool) {
        self.regions.push(Region {
            id: self.next_id,
            bounds,
            seen: now,
            decoded,
            drift: 0.0,
            hits: 0,
        });
        self.next_id += 1;
        if self.regions.len() > MAX_REGIONS {
            self.regions
                .sort_by(|a, b| b.decoded.cmp(&a.decoded).then(b.seen.total_cmp(&a.seen)));
            self.regions.truncate(MAX_REGIONS);
        }
    }

    /// Decide what to decode from one `width`×`height` frame with `slots`
    /// free decoders: whether to run a full-frame scan, and up to `slots`
    /// crops (one fewer while scanning). Crops rotate across calls so a short
    /// pool does not starve the same region every frame.
    pub fn plan(&mut self, now: f64, slots: usize, width: usize, height: usize) -> Plan {
        self.regions
            .retain(|region| now - region.seen <= REGION_TTL_MS);
        let live = self.decoded_count();
        if live >= self.expected || now - self.expected_at > EXPECTED_DECAY_MS {
            self.expected = live;
            self.expected_at = now;
        }
        let interval = if live == 0 {
            ACQUIRE_SCAN_MS
        } else if live < self.expected {
            DEGRADED_SCAN_MS
        } else {
            LOCKED_SCAN_MS
        };
        let full = slots > 0 && now - self.last_full_scan >= interval;
        if full {
            self.last_full_scan = now;
        }

        let mut crops = Vec::new();
        let mut free = slots - usize::from(full);
        let count = self.regions.len();
        for index in 0..count {
            if free == 0 {
                break;
            }
            let region = &self.regions[(index + self.rotate) % count];
            let b = region.bounds;
            let size = b.w.max(b.h);
            let pad = (size * REGION_PAD + size.min(2.0 * region.drift)).round();
            let x = (b.x - pad).floor().max(0.0) as usize;
            let y = (b.y - pad).floor().max(0.0) as usize;
            if x >= width || y >= height {
                continue;
            }
            let w = (width - x).min((b.w + 2.0 * pad).ceil().max(0.0) as usize);
            let h = (height - y).min((b.h + 2.0 * pad).ceil().max(0.0) as usize);
            if w < MIN_CROP || h < MIN_CROP {
                continue;
            }
            crops.push(Crop { x, y, w, h });
            free -= 1;
        }
        self.rotate = self.rotate.wrapping_add(1);
        Plan { full, crops }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f32, y: f32, size: f32) -> Bounds {
        Bounds {
            x,
            y,
            w: size,
            h: size,
        }
    }

    #[test]
    fn scans_every_100_ms_until_something_decodes() {
        let mut tracker = RegionTracker::new();
        assert!(tracker.plan(0.0, 4, 1280, 960).full);
        assert!(!tracker.plan(50.0, 4, 1280, 960).full);
        assert!(tracker.plan(100.0, 4, 1280, 960).full);
        assert_eq!(tracker.mode(), Mode::Acquiring);
    }

    #[test]
    fn a_locked_code_gets_a_padded_crop_and_rare_scans() {
        let mut tracker = RegionTracker::new();
        tracker.plan(0.0, 4, 1280, 960);
        tracker.note_decoded(bounds(400.0, 300.0, 200.0), 10.0);
        let plan = tracker.plan(20.0, 4, 1280, 960);
        assert!(!plan.full, "locked: the next scan is 1500 ms out");
        assert_eq!(tracker.mode(), Mode::Locked);
        // 35% of 200 px of padding on every side.
        assert_eq!(
            plan.crops,
            [Crop {
                x: 330,
                y: 230,
                w: 340,
                h: 340
            }]
        );
        assert!(tracker.plan(1500.0, 4, 1280, 960).full);
    }

    #[test]
    fn a_missing_code_degrades_the_cadence_to_250_ms() {
        let mut tracker = RegionTracker::new();
        tracker.note_decoded(bounds(100.0, 100.0, 200.0), 0.0);
        tracker.note_decoded(bounds(700.0, 100.0, 200.0), 0.0);
        tracker.plan(0.0, 4, 1280, 960);
        assert_eq!(tracker.expected(), 2);
        // Only one keeps decoding; the other ages out.
        for step in 1..=8 {
            tracker.note_decoded(bounds(100.0, 100.0, 200.0), f64::from(step) * 200.0);
        }
        let plan = tracker.plan(1700.0, 4, 1280, 960);
        assert_eq!(tracker.mode(), Mode::Degraded);
        assert!(plan.full);
        assert!(!tracker.plan(1800.0, 4, 1280, 960).full);
        assert!(tracker.plan(1950.0, 4, 1280, 960).full);
    }

    #[test]
    fn a_scan_takes_one_slot_and_crops_the_rest_rotating() {
        let mut tracker = RegionTracker::new();
        for index in 0..4 {
            tracker.note_decoded(bounds(index as f32 * 300.0, 0.0, 200.0), 0.0);
        }
        let plan = tracker.plan(1.0, 3, 1280, 960);
        assert!(plan.full);
        assert_eq!(plan.crops.len(), 2);
        let next = tracker.plan(2.0, 2, 1280, 960);
        assert!(!next.full);
        assert_eq!(next.crops.len(), 2);
        assert_ne!(plan.crops[0], next.crops[0], "the crop order rotates");
        assert_eq!(tracker.plan(3.0, 0, 1280, 960), Plan::default());
    }

    #[test]
    fn sightings_keep_regions_alive_but_never_move_or_found_them_alone() {
        let mut tracker = RegionTracker::new();
        tracker.note_sighting(bounds(0.0, 0.0, 200.0), 0.0);
        assert!(tracker.regions().is_empty(), "no decoded reference yet");
        tracker.note_decoded(bounds(0.0, 0.0, 200.0), 0.0);
        tracker.note_sighting(bounds(20.0, 20.0, 150.0), 1000.0);
        assert_eq!(tracker.regions()[0].bounds, bounds(0.0, 0.0, 200.0));
        assert_eq!(tracker.regions()[0].seen, 1000.0);
        tracker.note_sighting(bounds(900.0, 600.0, 40.0), 1000.0);
        assert_eq!(tracker.regions().len(), 1, "a much smaller box is noise");
        tracker.note_sighting(bounds(900.0, 600.0, 220.0), 1000.0);
        assert_eq!(tracker.regions().len(), 2);
        tracker.plan(2600.0, 4, 1280, 960);
        assert!(tracker.regions().is_empty(), "both age out after 1.5 s");
    }
}
