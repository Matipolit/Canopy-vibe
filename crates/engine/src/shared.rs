use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use project::TrackId;

/// State the audio thread publishes for the UI. Every field is atomic, so
/// neither side ever blocks.
#[derive(Debug, Default)]
pub struct Shared {
    peaks: [PeakMeter; 2],
    /// Transport position in samples.
    playhead: AtomicU64,
    /// The highest share of its time the audio callback spent rendering
    /// since the UI last looked: 1 means it only just kept up.
    load: PeakMeter,
}

impl Shared {
    /// Peak level of each channel since the last call, as linear amplitude.
    pub fn take_peaks(&self) -> [f32; 2] {
        [self.peaks[0].take(), self.peaks[1].take()]
    }

    /// The busiest callback since the last call: the time it took over the
    /// time its audio lasts.
    pub fn take_load(&self) -> f32 {
        self.load.take()
    }

    pub(crate) fn record_load(&self, load: f32) {
        self.load.record(load);
    }

    pub fn playhead(&self) -> u64 {
        self.playhead.load(Ordering::Relaxed)
    }

    pub(crate) fn publish(&self, peaks: [f32; 2], playhead: u64) {
        self.peaks[0].record(peaks[0]);
        self.peaks[1].record(peaks[1]);
        self.playhead.store(playhead, Ordering::Relaxed);
    }
}

/// Every track's output level. Each snapshot brings its own, laid out like
/// its track list, so the audio thread indexes it directly.
#[derive(Debug)]
pub struct TrackMeters {
    ids: Box<[TrackId]>,
    peaks: Box<[[PeakMeter; 2]]>,
}

impl TrackMeters {
    pub(crate) fn new(ids: Vec<TrackId>) -> TrackMeters {
        let peaks = ids.iter().map(|_| Default::default()).collect();
        TrackMeters {
            ids: ids.into_boxed_slice(),
            peaks,
        }
    }

    pub(crate) fn record(&self, index: usize, peaks: [f32; 2]) {
        if let Some([left, right]) = self.peaks.get(index) {
            left.record(peaks[0]);
            right.record(peaks[1]);
        }
    }

    /// Each track's peaks since the last call.
    pub fn take(&self) -> impl Iterator<Item = (TrackId, [f32; 2])> + '_ {
        self.ids
            .iter()
            .zip(self.peaks.iter())
            .map(|(id, [l, r])| (*id, [l.take(), r.take()]))
    }
}

/// The highest peak since the UI last looked. The audio thread raises it with
/// `fetch_max` and the UI reads and clears it with `swap`, so a transient that
/// lands between two UI frames still shows up.
///
/// This compares the raw bits: for non-negative floats, IEEE 754 bit patterns
/// sort in the same order as the values they represent.
#[derive(Debug, Default)]
pub struct PeakMeter(AtomicU32);

impl PeakMeter {
    pub fn record(&self, peak: f32) {
        let peak = peak.abs();
        if peak.is_finite() {
            self.0.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_meter_keeps_the_max_until_taken() {
        let meter = PeakMeter::default();
        meter.record(0.25);
        meter.record(-0.75);
        meter.record(0.5);
        assert_eq!(meter.take(), 0.75);
        assert_eq!(meter.take(), 0.0);
    }

    #[test]
    fn peak_meter_ignores_nan() {
        let meter = PeakMeter::default();
        meter.record(0.5);
        meter.record(f32::NAN);
        assert_eq!(meter.take(), 0.5);
    }

    #[test]
    fn track_meters_are_per_track() {
        let meters = TrackMeters::new(vec![TrackId(4), TrackId(9)]);
        meters.record(1, [0.5, 0.25]);
        meters.record(7, [1.0, 1.0]); // out of range: ignored
        let taken: Vec<_> = meters.take().collect();
        assert_eq!(taken, [(TrackId(4), [0.0, 0.0]), (TrackId(9), [0.5, 0.25])]);
    }
}
