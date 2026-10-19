//! The VST 2.4 binary interface: the structures, function types and
//! numbers a host and a plugin exchange, as far as this crate uses them.
//!
//! This is a clean-room description, written from public accounts of the
//! format (how hosts drive VST2 plugins, and the layout facts every VST2
//! host and plugin agree on), not a copy of Steinberg's SDK headers, which
//! were never freely licensed and are no longer distributed, nor of the
//! GPL reimplementations of them. Names follow the conventional ones so the
//! code reads against any description of the format.
//!
//! Only 64-bit layouts are described: pointer-sized fields are `isize` or
//! pointers. The tests at the bottom pin every size and offset the host
//! relies on. The test plugin (`test-plugins/vst2-gain`) includes this file
//! too.

#![allow(non_upper_case_globals, non_camel_case_types, dead_code)]

use std::ffi::c_void;

/// `'VstP'`: what every `AEffect` starts with.
pub const kEffectMagic: i32 = i32::from_be_bytes(*b"VstP");

/// The VST version this host speaks (2.4), answered to
/// `audioMasterVersion`.
pub const kVstVersion: isize = 2400;

/// The plugin's general entry: takes an opcode and its arguments (whose
/// meaning depends on the opcode) and returns a result.
pub type Dispatcher = unsafe extern "C" fn(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize;
/// `processReplacing` (and the old accumulating `process`): one pointer
/// per input and output channel, and the block length.
pub type ProcessFn = unsafe extern "C" fn(
    effect: *mut AEffect,
    inputs: *mut *mut f32,
    outputs: *mut *mut f32,
    frames: i32,
);
pub type ProcessDoubleFn = unsafe extern "C" fn(
    effect: *mut AEffect,
    inputs: *mut *mut f64,
    outputs: *mut *mut f64,
    frames: i32,
);
pub type SetParameterFn = unsafe extern "C" fn(effect: *mut AEffect, index: i32, value: f32);
pub type GetParameterFn = unsafe extern "C" fn(effect: *mut AEffect, index: i32) -> f32;
/// The host's entry (`audioMaster`), shaped like the dispatcher.
pub type HostCallback = unsafe extern "C" fn(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize;
/// `VSTPluginMain` (or `main` in older plugins): makes one plugin.
pub type MainFn = unsafe extern "C" fn(host: HostCallback) -> *mut AEffect;

/// One plugin as the host sees it. The plugin owns it; it lives from the
/// entry call until `effClose`.
#[repr(C)]
pub struct AEffect {
    /// [`kEffectMagic`].
    pub magic: i32,
    pub dispatcher: Option<Dispatcher>,
    /// The accumulating process call VST 2.4 deprecated.
    pub process: Option<ProcessFn>,
    pub set_parameter: Option<SetParameterFn>,
    pub get_parameter: Option<GetParameterFn>,
    pub num_programs: i32,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    /// `effFlags*`.
    pub flags: i32,
    /// Reserved for the host: this crate keeps its state's address here.
    pub resvd1: isize,
    pub resvd2: isize,
    /// The plugin's latency in samples, valid while it's resumed.
    pub initial_delay: i32,
    pub real_qualities: i32,
    pub off_qualities: i32,
    pub io_ratio: f32,
    /// The plugin's own pointer to itself.
    pub object: *mut c_void,
    pub user: *mut c_void,
    /// Identifies the plugin (often four characters, like `'Vtal'`).
    pub unique_id: i32,
    pub version: i32,
    pub process_replacing: Option<ProcessFn>,
    pub process_double_replacing: Option<ProcessDoubleFn>,
    pub future: [u8; 56],
}

// `AEffect::flags`.
pub const effFlagsHasEditor: i32 = 1 << 0;
pub const effFlagsCanReplacing: i32 = 1 << 4;
/// State is an opaque chunk (`effGetChunk`), not just parameter values.
pub const effFlagsProgramChunks: i32 = 1 << 5;
pub const effFlagsIsSynth: i32 = 1 << 8;
pub const effFlagsNoSoundInStop: i32 = 1 << 9;
pub const effFlagsCanDoubleReplacing: i32 = 1 << 12;

// Dispatcher opcodes: the host calling the plugin.
pub const effOpen: i32 = 0;
/// The last call: the plugin frees itself.
pub const effClose: i32 = 1;
/// `value`: the program.
pub const effSetProgram: i32 = 2;
pub const effGetProgram: i32 = 3;
/// `ptr`: a buffer for the name.
pub const effGetProgramName: i32 = 5;
/// `index`: the parameter; `ptr`: a buffer for its unit ("dB").
pub const effGetParamLabel: i32 = 6;
/// `index`: the parameter; `ptr`: a buffer for its current value as text.
pub const effGetParamDisplay: i32 = 7;
/// `index`: the parameter; `ptr`: a buffer for its name.
pub const effGetParamName: i32 = 8;
/// `opt`: the sample rate.
pub const effSetSampleRate: i32 = 10;
/// `value`: the longest block.
pub const effSetBlockSize: i32 = 11;
/// `value`: 1 to resume (activate), 0 to suspend.
pub const effMainsChanged: i32 = 12;
/// `ptr`: where to write a pointer to the editor's [`ERect`].
pub const effEditGetRect: i32 = 13;
/// `ptr`: the parent window (an X11 window id, or an `HWND`).
pub const effEditOpen: i32 = 14;
pub const effEditClose: i32 = 15;
/// Lets the editor do its work, from the host's GUI thread.
pub const effEditIdle: i32 = 19;
/// `index`: 0 for the bank, 1 for the program; `ptr`: where to write a
/// pointer to the chunk, valid until the next call. Returns its size.
pub const effGetChunk: i32 = 23;
/// `index`: as for [`effGetChunk`]; `value`: the size; `ptr`: the chunk.
pub const effSetChunk: i32 = 24;
/// `ptr`: a [`VstEvents`] for the next process call.
pub const effProcessEvents: i32 = 25;
pub const effCanBeAutomated: i32 = 26;
/// `index`: the parameter; `ptr`: text. Sets the parameter from the text.
pub const effString2Parameter: i32 = 27;
pub const effGetProgramNameIndexed: i32 = 29;
/// Returns a `kPlugCateg*`.
pub const effGetPlugCategory: i32 = 35;
/// `ptr`: a buffer for the name.
pub const effGetEffectName: i32 = 45;
pub const effGetVendorString: i32 = 47;
pub const effGetProductString: i32 = 48;
pub const effGetVendorVersion: i32 = 49;
/// `ptr`: a capability string. Returns 1 yes, -1 no, 0 don't know.
pub const effCanDo: i32 = 51;
/// Returns the tail in samples: 0 unknown, 1 none.
pub const effGetTailSize: i32 = 52;
/// `index`: the parameter; `ptr`: a [`VstParameterProperties`] to fill.
/// Returns 1 if the plugin did.
pub const effGetParameterProperties: i32 = 56;
pub const effGetVstVersion: i32 = 58;
pub const effBeginSetProgram: i32 = 67;
pub const effEndSetProgram: i32 = 68;
/// For shells: `ptr` gets the next sub-plugin's name; returns its id.
pub const effShellGetNextPlugin: i32 = 70;
pub const effStartProcess: i32 = 71;
pub const effStopProcess: i32 = 72;
/// `value`: a `kVstProcessPrecision*`.
pub const effSetProcessPrecision: i32 = 77;

// Callback opcodes: the plugin calling the host.
/// `index`: the parameter; `opt`: its new value, set by the plugin itself.
pub const audioMasterAutomate: i32 = 0;
pub const audioMasterVersion: i32 = 1;
/// The id of the plugin being loaded (what shells make).
pub const audioMasterCurrentId: i32 = 2;
pub const audioMasterIdle: i32 = 3;
pub const audioMasterWantMidi: i32 = 6;
/// `value`: the `kVst*Valid` fields wanted. Returns a [`VstTimeInfo`]
/// pointer.
pub const audioMasterGetTime: i32 = 7;
/// `ptr`: a [`VstEvents`] the plugin sends out, during processing.
pub const audioMasterProcessEvents: i32 = 8;
/// Inputs, outputs or latency (`initialDelay`) changed.
pub const audioMasterIOChanged: i32 = 13;
pub const audioMasterNeedIdle: i32 = 14;
/// `index`: width; `value`: height.
pub const audioMasterSizeWindow: i32 = 15;
pub const audioMasterGetSampleRate: i32 = 16;
pub const audioMasterGetBlockSize: i32 = 17;
pub const audioMasterGetInputLatency: i32 = 18;
pub const audioMasterGetOutputLatency: i32 = 19;
/// Returns a `kVstProcessLevel*`: which kind of thread is calling.
pub const audioMasterGetCurrentProcessLevel: i32 = 23;
pub const audioMasterGetAutomationState: i32 = 24;
pub const audioMasterGetVendorString: i32 = 32;
pub const audioMasterGetProductString: i32 = 33;
pub const audioMasterGetVendorVersion: i32 = 34;
pub const audioMasterVendorSpecific: i32 = 35;
/// `ptr`: a capability string. Returns 1 yes, 0 no.
pub const audioMasterCanDo: i32 = 37;
pub const audioMasterGetLanguage: i32 = 38;
pub const audioMasterGetDirectory: i32 = 41;
/// Something changed that the host shows (programs, parameter values).
pub const audioMasterUpdateDisplay: i32 = 42;
/// `index`: the parameter the user starts moving in the editor.
pub const audioMasterBeginEdit: i32 = 43;
pub const audioMasterEndEdit: i32 = 44;

// `effGetPlugCategory`.
pub const kPlugCategUnknown: isize = 0;
pub const kPlugCategEffect: isize = 1;
pub const kPlugCategSynth: isize = 2;
pub const kPlugCategAnalysis: isize = 3;
pub const kPlugCategMastering: isize = 4;
pub const kPlugCategSpacializer: isize = 5;
pub const kPlugCategRoomFx: isize = 6;
pub const kPlugSurroundFx: isize = 7;
pub const kPlugCategRestoration: isize = 8;
pub const kPlugCategOfflineProcess: isize = 9;
pub const kPlugCategShell: isize = 10;
pub const kPlugCategGenerator: isize = 11;

// `audioMasterGetCurrentProcessLevel`.
pub const kVstProcessLevelUnknown: isize = 0;
/// The GUI (main) thread.
pub const kVstProcessLevelUser: isize = 1;
/// The audio thread, in real time.
pub const kVstProcessLevelRealtime: isize = 2;
pub const kVstProcessLevelPrefetch: isize = 3;
/// The audio thread of an offline render.
pub const kVstProcessLevelOffline: isize = 4;

// `audioMasterGetAutomationState`.
pub const kVstAutomationUnsupported: isize = 0;
pub const kVstAutomationOff: isize = 1;
pub const kVstAutomationRead: isize = 2;
pub const kVstAutomationWrite: isize = 3;
pub const kVstAutomationReadWrite: isize = 4;

// `effSetProcessPrecision`.
pub const kVstProcessPrecision32: isize = 0;
pub const kVstProcessPrecision64: isize = 1;

pub const kVstLangEnglish: isize = 1;

// The sizes the format gives string buffers. Plugins overrun the short
// ones often enough that the host passes bigger buffers.
pub const kVstMaxProgNameLen: usize = 24;
pub const kVstMaxParamStrLen: usize = 8;
pub const kVstMaxVendorStrLen: usize = 64;
pub const kVstMaxProductStrLen: usize = 64;
pub const kVstMaxEffectNameLen: usize = 32;

/// Any event: [`VstMidiEvent`] or a SysEx one, told apart by `type_`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VstEvent {
    pub type_: i32,
    pub byte_size: i32,
    pub delta_frames: i32,
    pub flags: i32,
    pub data: [u8; 16],
}

pub const kVstMidiType: i32 = 1;
pub const kVstSysExType: i32 = 6;

/// A MIDI message at a sample of the next (or current) process call.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VstMidiEvent {
    /// [`kVstMidiType`].
    pub type_: i32,
    /// `size_of::<VstMidiEvent>()`.
    pub byte_size: i32,
    /// The sample the message happens at, from the block's start.
    pub delta_frames: i32,
    pub flags: i32,
    pub note_length: i32,
    pub note_offset: i32,
    /// Status and data bytes; the fourth is 0.
    pub midi_data: [u8; 4],
    pub detune: i8,
    pub note_off_velocity: u8,
    pub reserved1: u8,
    pub reserved2: u8,
}

/// A list of events: `num_events` pointers follow `reserved` (the header
/// declares two; the list is as long as it needs to be).
#[repr(C)]
pub struct VstEvents {
    pub num_events: i32,
    pub reserved: isize,
    pub events: [*mut VstEvent; 2],
}

/// Where the song is, for `audioMasterGetTime`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VstTimeInfo {
    pub sample_pos: f64,
    pub sample_rate: f64,
    pub nano_seconds: f64,
    /// Quarter notes from the song's start.
    pub ppq_pos: f64,
    pub tempo: f64,
    /// Where the current bar started, in quarter notes.
    pub bar_start_pos: f64,
    pub cycle_start_pos: f64,
    pub cycle_end_pos: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub smpte_offset: i32,
    pub smpte_frame_rate: i32,
    pub samples_to_next_clock: i32,
    /// `kVst*` flags: what's valid, and the transport's state.
    pub flags: i32,
}

pub const kVstTransportChanged: i32 = 1;
pub const kVstTransportPlaying: i32 = 1 << 1;
pub const kVstTransportCycleActive: i32 = 1 << 2;
pub const kVstTransportRecording: i32 = 1 << 3;
pub const kVstAutomationWriting: i32 = 1 << 6;
pub const kVstAutomationReading: i32 = 1 << 7;
pub const kVstNanosValid: i32 = 1 << 8;
pub const kVstPpqPosValid: i32 = 1 << 9;
pub const kVstTempoValid: i32 = 1 << 10;
pub const kVstBarsValid: i32 = 1 << 11;
pub const kVstCyclePosValid: i32 = 1 << 12;
pub const kVstTimeSigValid: i32 = 1 << 13;
pub const kVstSmpteValid: i32 = 1 << 14;
pub const kVstClockValid: i32 = 1 << 15;

/// An editor's rectangle, in pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ERect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

/// What `effGetParameterProperties` says about a parameter.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VstParameterProperties {
    pub step_float: f32,
    pub small_step_float: f32,
    pub large_step_float: f32,
    pub label: [u8; 64],
    /// `kVstParameter*`.
    pub flags: i32,
    pub min_integer: i32,
    pub max_integer: i32,
    pub step_integer: i32,
    pub large_step_integer: i32,
    pub short_label: [u8; 8],
    pub display_index: i16,
    /// 1 and up: the category it's in; 0 for none.
    pub category: i16,
    pub num_parameters_in_category: i16,
    pub reserved: i16,
    pub category_label: [u8; 24],
    pub future: [u8; 16],
}

pub const kVstParameterIsSwitch: i32 = 1 << 0;
pub const kVstParameterUsesIntegerMinMax: i32 = 1 << 1;
pub const kVstParameterUsesFloatStep: i32 = 1 << 2;
pub const kVstParameterUsesIntStep: i32 = 1 << 3;
pub const kVstParameterSupportsDisplayIndex: i32 = 1 << 4;
pub const kVstParameterSupportsDisplayCategory: i32 = 1 << 5;
pub const kVstParameterCanRamp: i32 = 1 << 6;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_match_the_64_bit_abi() {
        assert_eq!(kEffectMagic, 0x5673_7450);
        assert_eq!(size_of::<AEffect>(), 192);
        assert_eq!(offset_of!(AEffect, dispatcher), 8);
        assert_eq!(offset_of!(AEffect, get_parameter), 32);
        assert_eq!(offset_of!(AEffect, num_programs), 40);
        assert_eq!(offset_of!(AEffect, flags), 56);
        assert_eq!(offset_of!(AEffect, resvd1), 64);
        assert_eq!(offset_of!(AEffect, initial_delay), 80);
        assert_eq!(offset_of!(AEffect, object), 96);
        assert_eq!(offset_of!(AEffect, unique_id), 112);
        assert_eq!(offset_of!(AEffect, process_replacing), 120);
        assert_eq!(offset_of!(AEffect, process_double_replacing), 128);
        assert_eq!(size_of::<VstEvent>(), 32);
        assert_eq!(size_of::<VstMidiEvent>(), 32);
        assert_eq!(offset_of!(VstMidiEvent, midi_data), 24);
        assert_eq!(offset_of!(VstMidiEvent, note_off_velocity), 29);
        assert_eq!(offset_of!(VstEvents, events), 16);
        assert_eq!(size_of::<VstTimeInfo>(), 88);
        assert_eq!(offset_of!(VstTimeInfo, time_sig_numerator), 64);
        assert_eq!(offset_of!(VstTimeInfo, flags), 84);
        assert_eq!(size_of::<ERect>(), 8);
        assert_eq!(size_of::<VstParameterProperties>(), 152);
        assert_eq!(offset_of!(VstParameterProperties, flags), 76);
        assert_eq!(offset_of!(VstParameterProperties, category), 106);
        assert_eq!(offset_of!(VstParameterProperties, category_label), 112);
    }
}
