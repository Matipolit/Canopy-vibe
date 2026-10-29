//! How long loading a file takes, step by step: decoding, peaks, and
//! resampling to a rate.
//!
//! ```sh
//! cargo run --release -p media --example load_time -- file.wav [rate]
//! ```

use std::path::Path;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a file");
    let rate: u32 = args.next().map_or(44_100, |r| r.parse().expect("a rate"));
    let path = Path::new(&path);
    for _ in 0..3 {
        let start = Instant::now();
        let (native, channels) = media::decode(path).expect("decodes");
        let decoded = start.elapsed();
        let start = Instant::now();
        let peaks = media::Peaks::new(&native);
        let peaked = start.elapsed();
        let start = Instant::now();
        let resampled = media::resample(&native, rate);
        let resampling = start.elapsed();
        let start = Instant::now();
        let whole = media::load(path, rate).expect("loads");
        let loading = start.elapsed();
        println!(
            "{} frames x {channels} at {} Hz: decode {:.1} ms, peaks {:.1} ms, resample to {rate} {:.1} ms; load {:.1} ms",
            native.frames(),
            native.sample_rate(),
            decoded.as_secs_f64() * 1e3,
            peaked.as_secs_f64() * 1e3,
            resampling.as_secs_f64() * 1e3,
            loading.as_secs_f64() * 1e3,
        );
        drop((peaks, resampled, whole));
    }
}
