//! Reads one plugin file and prints what's in it, for [`plugins::scan`] to
//! run in a separate process: `canopy-plugin-scanner --canopy-plugin-scan
//! <clap|vst3|vst2> <path>`. Apps that can't start themselves as a scanner (see
//! `plugins::scan::run_scanner_if_requested`) ship this next to them.

fn main() {
    plugins::scan::run_scanner_if_requested();
    eprintln!(
        "usage: canopy-plugin-scanner {} <clap|vst3|vst2> <path>",
        plugins::scan::SCAN_ARG
    );
    std::process::exit(2);
}
