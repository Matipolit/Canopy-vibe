//! Loading SFZ instruments: parsing, inheritance, paths, includes and the
//! warnings the UI shows. Samples come from a fake decoder.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use instruments::builtin::sampler::{self, Decoded, LoadError, RegionInfo, SfzBank, Trigger};

const ROOT: &str = "/library/Piano/piano.sfz";

fn sine(frames: usize) -> Decoded {
    let wave = (0..frames).map(|i| (i as f32 * 0.1).sin()).collect();
    Decoded::mono(wave, 44_100)
}

/// Loads `text` as if it were `path`, with every sample decoding to a short
/// sine except names containing "missing". Returns the paths decoded.
fn load_at(text: &str, path: &Path) -> (Result<SfzBank, LoadError>, Vec<PathBuf>) {
    let requests = RefCell::new(Vec::new());
    let decode = |path: &Path| {
        requests.borrow_mut().push(path.to_path_buf());
        if path.to_string_lossy().contains("missing") {
            Err("no such file".to_owned())
        } else {
            Ok(sine(1000))
        }
    };
    let bank = sampler::load_str(text, path, &decode);
    (bank, requests.into_inner())
}

fn load(text: &str) -> SfzBank {
    let (bank, _) = load_at(text, Path::new(ROOT));
    bank.unwrap()
}

fn ranges(region: &RegionInfo) -> (u8, u8, u8, u8, u8) {
    (
        region.lokey,
        region.hikey,
        region.lovel,
        region.hivel,
        region.root_key,
    )
}

/// A fresh directory under Cargo's temp dir for files a test writes.
fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn headers_inherit_global_master_group_region() {
    let bank = load(
        "<global> lovel=10 pitch_keycenter=c4
         <master> hivel=100
         <group> lokey=40 hikey=50
         <region> sample=a.wav
         <region> sample=b.wav lokey=45 hivel=127
         <group> key=d4
         <region> sample=c.wav
         <master>
         <region> sample=d.wav
         <global>
         <region> sample=e.wav",
    );
    let regions = bank.regions();
    assert_eq!(regions.len(), 5);
    assert_eq!(ranges(&regions[0]), (40, 50, 10, 100, 60));
    assert_eq!(ranges(&regions[1]), (45, 50, 10, 127, 60));
    // `key` sets the root too.
    assert_eq!(ranges(&regions[2]), (62, 62, 10, 100, 62));
    // A new master drops the group; a new global drops everything.
    assert_eq!(ranges(&regions[3]), (0, 127, 10, 127, 60));
    assert_eq!(ranges(&regions[4]), (0, 127, 0, 127, 60));
    assert_eq!(
        regions
            .iter()
            .map(|r| r.sample.as_str())
            .collect::<Vec<_>>(),
        ["a.wav", "b.wav", "c.wav", "d.wav", "e.wav"]
    );
    assert!(bank.warnings().is_empty(), "{:?}", bank.warnings());
}

#[test]
fn defines_and_note_names() {
    let bank = load(
        "#define $LOW c#3
         #define $HIGH 64
         #define $NAME Cello Sustain
         <region> sample=$NAME C3.wav lokey=$LOW hikey=$HIGH pitch_keycenter=db3
         <region> sample=x.wav key=a-1
         <region> sample=y.wav lokey=Bb3 hikey=b3 pitch_keycenter=G9",
    );
    let regions = bank.regions();
    assert_eq!(regions[0].sample, "Cello Sustain C3.wav");
    assert_eq!(ranges(&regions[0]), (49, 64, 0, 127, 49));
    assert_eq!(ranges(&regions[1]), (9, 9, 0, 127, 9));
    assert_eq!(ranges(&regions[2]), (58, 59, 0, 127, 127));
}

#[test]
fn control_note_and_octave_offsets_shift_keys() {
    let bank =
        load("<control> octave_offset=1 note_offset=-2 <region> sample=a.wav key=60 lokey=c3");
    assert_eq!(ranges(&bank.regions()[0]), (58, 70, 0, 127, 70));
}

#[test]
fn sample_paths_join_default_path_with_backslashes_and_spaces() {
    let (bank, requests) = load_at(
        "<control> default_path=..\\Shared Samples\\
         <region> sample=Piano C4 soft.wav key=60
         <region> sample=Sub Dir\\Piano D4.wav   key=62 // comment
         <region> sample=Piano C4 soft.wav key=61",
        Path::new(ROOT),
    );
    let bank = bank.unwrap();
    assert_eq!(
        requests,
        [
            PathBuf::from("/library/Piano/../Shared Samples/Piano C4 soft.wav"),
            PathBuf::from("/library/Piano/../Shared Samples/Sub Dir/Piano D4.wav"),
        ],
        "each distinct sample is decoded once"
    );
    assert_eq!(bank.regions().len(), 3);
    assert_eq!(bank.sample_count(), 2);
    assert_eq!(bank.regions()[1].sample, "Sub Dir/Piano D4.wav");
    assert_eq!(bank.name(), "piano");
    assert_eq!(bank.path(), Path::new(ROOT));
}

#[test]
fn includes_read_files_relative_to_the_instrument() {
    let dir = temp_dir("sampler_includes");
    write(
        &dir.join("main.sfz"),
        "#define $VEL 64\n#include \"inc/groups.sfz\"\n<region> sample=last.wav key=70\n#include \"main.sfz\"",
    );
    // Relative to the root file's directory, then to the including file's.
    write(
        &dir.join("inc/groups.sfz"),
        "<group> lovel=$VEL\n#include \"inc/regions.sfz\"\n#include \"more.sfz\"",
    );
    write(
        &dir.join("inc/regions.sfz"),
        "<region> sample=from include.wav key=c5",
    );
    write(&dir.join("inc/more.sfz"), "<region> sample=more.wav key=71");

    let requests = RefCell::new(Vec::new());
    let decode = |path: &Path| {
        requests.borrow_mut().push(path.to_path_buf());
        Ok(sine(100))
    };
    let bank = sampler::load(&dir.join("main.sfz"), &decode).unwrap();
    let regions = bank.regions();
    assert_eq!(
        regions
            .iter()
            .map(|r| (r.sample.as_str(), r.lokey, r.lovel))
            .collect::<Vec<_>>(),
        [
            ("from include.wav", 72, 64),
            ("more.wav", 71, 64),
            ("last.wav", 70, 64)
        ]
    );
    assert_eq!(requests.borrow()[0], dir.join("from include.wav"));
    let warnings = bank.warnings();
    assert!(
        warnings.iter().any(|w| w.contains("includes itself")),
        "{warnings:?}"
    );
}

#[test]
fn sample_names_match_files_regardless_of_letter_case() {
    let dir = temp_dir("sampler_case");
    write(&dir.join("Samples/Piano C4.WAV"), "not really audio");
    let requests = RefCell::new(Vec::new());
    let decode = |path: &Path| {
        requests.borrow_mut().push(path.to_path_buf());
        Ok(sine(100))
    };
    let text = "<region> sample=samples\\piano c4.wav";
    sampler::load_str(text, &dir.join("kit.sfz"), &decode).unwrap();
    assert_eq!(requests.into_inner(), [dir.join("Samples/Piano C4.WAV")]);
}

#[test]
fn problems_become_warnings_not_failures() {
    let (bank, _) = load_at(
        "<region> sample=ok.wav amplitude_oncc11=50 foo=1
         <region> sample=missing.wav
         <region> sample=ok.wav lokey=zz
         <region> key=60
         <effect> type=reverb
         <region> sample=ok.wav hikey=-1
         <region> sample=ok.wav loop_mode=loop_continuous loop_start=5000 loop_end=6000",
        Path::new(ROOT),
    );
    let bank = bank.unwrap();
    assert_eq!(bank.regions().len(), 3);
    let warnings = bank.warnings().join("\n");
    for expected in [
        "Missing sample `missing.wav`: no such file",
        "bad value `zz` for `lokey`",
        "Unsupported, ignored: `amplitude_oncc11`",
        "Unsupported, ignored: `foo`",
        "Unsupported, ignored: `<effect>`",
        "Regions without a sample, skipped (1)",
        "Regions skipped for a missing sample (1)",
        "Regions played by no key (CC-triggered?), skipped (1)",
        "is outside the sample; not looped",
    ] {
        assert!(
            warnings.contains(expected),
            "no `{expected}` in:\n{warnings}"
        );
    }
    assert!(!bank.regions()[2].looped);
}

#[test]
fn nothing_playable_is_an_error() {
    let (result, _) = load_at("<region> sample=missing.wav", Path::new(ROOT));
    let Err(LoadError::NoRegions { warnings }) = result else {
        panic!("expected NoRegions, got {result:?}");
    };
    assert_eq!(warnings[0], "Missing sample `missing.wav`: no such file");

    let decode = |_: &Path| Ok(sine(10));
    let error = sampler::load(Path::new("/nowhere/at/all.sfz"), &decode).unwrap_err();
    assert!(matches!(error, LoadError::Read { .. }), "{error:?}");
    assert!(error.to_string().contains("/nowhere/at/all.sfz"));
}

#[test]
fn reports_memory_triggers_and_loops() {
    let decode = |path: &Path| {
        let name = path.file_name().unwrap().to_string_lossy();
        Ok(match name.as_ref() {
            "stereo.wav" => Decoded::stereo(vec![0.1; 48_000], vec![0.2; 48_000], 48_000),
            // A decoder's mono-to-stereo copy is stored once.
            "copied.wav" => Decoded::stereo(vec![0.1; 24_000], vec![0.1; 24_000], 48_000),
            _ => Decoded {
                loop_points: Some((100, 900)),
                ..sine(1000)
            },
        })
    };
    let text = "<region> sample=stereo.wav trigger=release seq_length=2 seq_position=2
                <region> sample=copied.wav sw_last=c1
                <region> sample=looped.wav";
    let bank = sampler::load_str(text, Path::new(ROOT), &decode).unwrap();
    let floats = 2 * 48_000 + 24_000 + 1000;
    let memory = bank.sample_memory_bytes();
    assert!(
        (floats * 4..floats * 4 + 200).contains(&memory),
        "{memory} bytes for {floats} floats"
    );
    let regions = bank.regions();
    assert_eq!(regions[0].trigger, Trigger::Release);
    assert_eq!((regions[0].seq_position, regions[0].seq_length), (2, 2));
    assert_eq!(regions[0].seconds, 1.0);
    assert_eq!(regions[1].keyswitch, Some(24));
    assert!(!regions[1].looped);
    // A file's own loop makes loop_continuous the default.
    assert!(regions[2].looped);
}

#[test]
fn latin1_text_and_comments() {
    let dir = temp_dir("sampler_latin1");
    let path = dir.join("old.sfz");
    // "Grüße.wav" in Latin-1, a block comment over two lines, CRLF endings.
    let mut bytes = b"/* made\r\nwith an old editor */\r\n<region> sample=Gr".to_vec();
    bytes.extend([0xFC, 0xDF, b'e']);
    bytes.extend(b".wav key=60 // the end\r\n");
    fs::write(&path, bytes).unwrap();
    let decode = |_: &Path| Ok(sine(10));
    let bank = sampler::load(&path, &decode).unwrap();
    assert_eq!(bank.regions()[0].sample, "Grüße.wav");
    assert!(bank.warnings().is_empty(), "{:?}", bank.warnings());
}
