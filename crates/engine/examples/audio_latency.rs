//! Opens the audio device as the app does, with an empty engine, and
//! reports what the stream does: frames per callback, how far ahead of the
//! speakers it writes (as the audio system predicts it), callbacks per
//! second, and the CPU the whole process spends meanwhile, which with
//! nothing to render is the cost of the audio path itself.
//!
//! ```sh
//! cargo run --release -p engine --example audio_latency -- [seconds] [--buffer 256] [--rate 48000]
//! CANOPY_AUDIO=alsa cargo run --release -p engine --example audio_latency   # force ALSA
//! ```
//!
//! While it runs, `pw-top -b -n 3` shows the graph's own view (QUANT, and
//! the node's wait and busy times).

use std::time::{Duration, Instant};

use engine::device::{self, AudioSettings};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut seconds = 10.0f64;
    let mut settings = AudioSettings::new(48_000);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--buffer" => settings.buffer_frames = number(args.next()),
            "--rate" => settings.sample_rate = number(args.next()),
            "--output" => settings.output = args.next(),
            other => seconds = other.parse().expect("seconds"),
        }
    }

    let (_handle, output) = device::start(&settings).expect("open the device");
    println!(
        "{} · {} · {} Hz, asked for {} frames",
        output.host_name, output.device_name, output.sample_rate, settings.buffer_frames
    );
    // Let it settle before measuring.
    std::thread::sleep(Duration::from_millis(500));

    let cpu_before = cpu_time();
    let first = output.reading();
    let start = Instant::now();
    let mut latencies = Vec::new();
    let mut frames = Vec::new();
    while start.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(20));
        let reading = output.reading();
        latencies.push(reading.latency.as_secs_f64() * 1000.0);
        frames.push(reading.frames);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cpu = cpu_time() - cpu_before;
    let last = output.reading();

    latencies.sort_by(f64::total_cmp);
    frames.sort_unstable();
    let pick = |v: &[f64], q: f64| v[((v.len() - 1) as f64 * q) as usize];
    println!(
        "frames per callback: {}..{} (median {})",
        frames[0],
        frames[frames.len() - 1],
        frames[frames.len() / 2]
    );
    println!(
        "predicted output latency: min {:.2} ms, median {:.2} ms, max {:.2} ms",
        pick(&latencies, 0.0),
        pick(&latencies, 0.5),
        pick(&latencies, 1.0)
    );
    if let Some(rate) = last.graph_rate {
        println!("graph rate: {rate} Hz");
    }
    println!(
        "callbacks: {:.1} per second",
        (last.callbacks - first.callbacks) as f64 / elapsed
    );
    println!(
        "process CPU: {:.3}% of a core ({:.1} ms over {elapsed:.1} s)",
        cpu / elapsed * 100.0,
        cpu * 1000.0
    );
}

fn number(arg: Option<String>) -> u32 {
    arg.and_then(|a| a.parse().ok()).expect("a number")
}

/// The process's user and system CPU time so far, in seconds, from
/// `/proc/self/stat` (in clock ticks, which Linux counts at 100 a second).
fn cpu_time() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    // The command name is in parentheses and may hold spaces: count after it.
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    ticks as f64 / 100.0
}
