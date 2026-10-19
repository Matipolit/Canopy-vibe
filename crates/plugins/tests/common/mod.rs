//! What the integration tests share: the test plugins, and the same
//! checks written once for every format.

#![allow(dead_code)]

use std::path::Path;

use plugins::fixtures::{self, Fixtures};
use plugins::{Host, ParamId, PluginFormat, PluginInfo, PluginInstance, scan};

#[allow(unused_imports)]
pub use plugins::fixtures::params::*;

pub fn fixtures() -> &'static Fixtures {
    fixtures::build().unwrap_or_else(|e| panic!("{e}"))
}

/// One format's test plugins, and its units.
#[derive(Debug, Clone, Copy)]
pub struct Format {
    pub format: PluginFormat,
    pub gain: &'static str,
    pub sine: &'static str,
}

pub const CLAP: Format = Format {
    format: PluginFormat::Clap,
    gain: fixtures::CLAP_GAIN,
    sine: fixtures::CLAP_SINE,
};

pub const VST3: Format = Format {
    format: PluginFormat::Vst3,
    gain: fixtures::VST3_GAIN,
    sine: fixtures::VST3_SINE,
};

pub const VST2: Format = Format {
    format: PluginFormat::Vst2,
    gain: fixtures::VST2_GAIN,
    sine: fixtures::VST2_SINE,
};

pub const FORMATS: [Format; 3] = [CLAP, VST3, VST2];

impl Format {
    /// The file holding plugin `id`.
    pub fn path(self, id: &str) -> &'static Path {
        let fixtures = fixtures();
        match self.format {
            PluginFormat::Clap => &fixtures.clap,
            PluginFormat::Vst3 => &fixtures.vst3,
            PluginFormat::Vst2 if id == fixtures::VST2_SINE => &fixtures.vst2_sine,
            PluginFormat::Vst2 => &fixtures.vst2_gain,
        }
    }

    /// Every file of this format's test plugins.
    pub fn files(self) -> Vec<&'static Path> {
        let mut files = vec![self.path(self.gain), self.path(self.sine)];
        files.dedup();
        files
    }

    pub fn info(self, id: &str) -> PluginInfo {
        scan::read_plugin_file(self.format, self.path(id))
            .unwrap()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
    }

    pub fn load(self, host: &Host, id: &str) -> PluginInstance {
        host.load(&self.info(id)).unwrap()
    }

    /// Gain's value for `db`.
    pub fn db(self, db: f64) -> f64 {
        match self.format {
            PluginFormat::Clap => db,
            PluginFormat::Vst3 | PluginFormat::Vst2 => (db + 60.0) / 72.0,
        }
    }

    /// Latency's value for `samples`.
    pub fn samples(self, samples: u32) -> f64 {
        match self.format {
            PluginFormat::Clap => f64::from(samples),
            PluginFormat::Vst3 | PluginFormat::Vst2 => f64::from(samples) / 1024.0,
        }
    }

    /// Tempo seen's value for `bpm`.
    pub fn bpm(self, bpm: f64) -> f64 {
        match self.format {
            PluginFormat::Clap => bpm,
            PluginFormat::Vst3 | PluginFormat::Vst2 => bpm / 1000.0,
        }
    }

    /// How close a value read back is to the one set: VST2 plugins keep
    /// them as 32-bit floats.
    pub fn eps(self) -> f64 {
        match self.format {
            PluginFormat::Vst2 => 1e-6,
            _ => 1e-9,
        }
    }
}

pub fn linear(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

pub fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

pub fn param(id: ParamId, frame: u32, value: f64) -> plugins::Event {
    plugins::Event::Param { frame, id, value }
}
