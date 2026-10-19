//! SFZ text to regions: comments, headers and their inheritance, `#define`,
//! `#include`, and the `<control>` settings that change sample paths and
//! note numbers. Opcode values stay text here; `region.rs` reads them.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Deeper than any real instrument nests includes; also stops cycles that
/// canonical paths miss.
const MAX_INCLUDE_DEPTH: usize = 16;

/// Individual warnings kept; past this they are only counted.
const MAX_MESSAGES: usize = 100;

/// One `name=value` as written, after `#define` substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Opcode {
    pub name: String,
    pub value: String,
}

/// A `<region>` with every opcode that applies to it, in the order they
/// apply: its `<global>`, `<master>` and `<group>`, then its own.
#[derive(Debug, Clone)]
pub(super) struct RawRegion {
    pub opcodes: Vec<Opcode>,
    /// `default_path` from `<control>` where the region was read.
    pub default_path: String,
    /// `note_offset + 12 * octave_offset` from `<control>`.
    pub note_offset: i32,
    /// Where its header is, like `piano.sfz:12`.
    pub location: String,
}

/// Problems found while loading. None of them stop the load; the UI lists
/// them.
#[derive(Debug, Default)]
pub(super) struct Warnings {
    messages: Vec<String>,
    seen: HashSet<String>,
    overflow: usize,
    /// Unsupported opcodes and headers, with how often they were met.
    unsupported: BTreeMap<String, usize>,
    /// Things worth a count rather than a line each.
    counts: BTreeMap<&'static str, usize>,
}

impl Warnings {
    pub fn add(&mut self, message: String) {
        if self.seen.contains(&message) {
            return;
        }
        if self.messages.len() < MAX_MESSAGES {
            self.seen.insert(message.clone());
            self.messages.push(message);
        } else {
            self.overflow += 1;
        }
    }

    /// A bad value, reported once per opcode and value however many regions
    /// inherit it.
    pub fn bad_value(&mut self, name: &str, value: &str, location: &str) {
        let key = format!("{name}={value}");
        if self.seen.insert(key) {
            self.add(format!("{location}: bad value `{value}` for `{name}`"));
        }
    }

    pub fn unsupported(&mut self, what: &str) {
        *self.unsupported.entry(what.to_owned()).or_default() += 1;
    }

    pub fn count(&mut self, what: &'static str) {
        *self.counts.entry(what).or_default() += 1;
    }

    pub fn finish(self) -> Vec<String> {
        let mut out = self.messages;
        if self.overflow > 0 {
            out.push(format!("…and {} more warnings", self.overflow));
        }
        for (what, count) in self.counts {
            out.push(format!("{what} ({count})"));
        }
        for (what, count) in self.unsupported {
            let times = if count == 1 {
                String::new()
            } else {
                format!(" ({count}×)")
            };
            out.push(format!("Unsupported, ignored: `{what}`{times}"));
        }
        out
    }
}

/// Opcodes that only matter to other hosts' UIs or MIDI setup, ignored
/// without a warning.
pub(super) fn is_ignored(name: &str) -> bool {
    name.starts_with("hint_")
        || name.starts_with("label_")
        || name.ends_with("_label")
        || name.starts_with("set_cc")
        || name.starts_with("set_hdcc")
        || name.starts_with("set_realcc")
        || matches!(name, "lochan" | "hichan")
}

/// Reads a text file. SFZ files from Windows tools are often Latin-1 rather
/// than UTF-8; those are read byte for byte.
pub(super) fn read_text(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    })
}

/// Parses `text`, the contents of `file`. Includes are read relative to
/// `file`'s directory (then the including file's).
pub(super) fn parse(text: &str, file: &Path, warnings: &mut Warnings) -> Vec<RawRegion> {
    let root_dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut parser = Parser {
        root_dir,
        warnings,
        defines: Vec::new(),
        section: Section::Start,
        global: Vec::new(),
        master: Vec::new(),
        group: Vec::new(),
        region: None,
        default_path: String::new(),
        note_offset: 0,
        octave_offset: 0,
        open_files: vec![fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf())],
        regions: Vec::new(),
    };
    parser.text(text, file, 0);
    parser.finish_region();
    parser.regions
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Before any header; opcodes here count as `<global>`.
    Start,
    Control,
    Global,
    Master,
    Group,
    Region,
    /// A header we don't support, like `<curve>`; its opcodes are skipped.
    Ignored,
}

#[derive(Clone, Copy)]
struct Location<'a> {
    file: &'a str,
    line: usize,
}

impl fmt::Display for Location<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

struct Parser<'w> {
    root_dir: PathBuf,
    warnings: &'w mut Warnings,
    /// Longest name first, so `$KEY` never replaces the start of `$KEYS`.
    defines: Vec<(String, String)>,
    section: Section,
    global: Vec<Opcode>,
    master: Vec<Opcode>,
    group: Vec<Opcode>,
    /// The region being read and where it started.
    region: Option<(Vec<Opcode>, String)>,
    default_path: String,
    note_offset: i32,
    octave_offset: i32,
    /// The files being read, outermost first, to catch include cycles.
    open_files: Vec<PathBuf>,
    regions: Vec<RawRegion>,
}

impl Parser<'_> {
    fn text(&mut self, text: &str, file: &Path, depth: usize) {
        let name = file.file_name().map_or_else(
            || file.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
        let text = strip_comments(text);
        for (index, line) in text.lines().enumerate() {
            let location = Location {
                file: &name,
                line: index + 1,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("#define") {
                self.define(rest, location);
                continue;
            }
            let line = self.substitute(line);
            if let Some(rest) = line.strip_prefix("#include") {
                self.include(rest, &dir, depth, location);
                continue;
            }
            self.line(&line, location);
        }
    }

    fn define(&mut self, rest: &str, location: Location) {
        let rest = rest.trim();
        let (name, value) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(name, value)| (name, value.trim()));
        if name.len() < 2 || !name.starts_with('$') {
            self.warnings
                .add(format!("{location}: `#define` needs a `$name` and a value"));
            return;
        }
        let value = self.substitute(value).into_owned();
        match self.defines.iter_mut().find(|(n, _)| n == name) {
            Some(define) => define.1 = value,
            None => {
                self.defines.push((name.to_owned(), value));
                self.defines
                    .sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
            }
        }
    }

    fn substitute<'t>(&self, text: &'t str) -> Cow<'t, str> {
        if self.defines.is_empty() || !text.contains('$') {
            return Cow::Borrowed(text);
        }
        let mut out = text.to_owned();
        for (name, value) in &self.defines {
            if out.contains(name.as_str()) {
                out = out.replace(name.as_str(), value);
            }
        }
        Cow::Owned(out)
    }

    fn include(&mut self, rest: &str, dir: &Path, depth: usize, location: Location) {
        let name = rest.trim().trim_matches('"').trim().replace('\\', "/");
        if name.is_empty() {
            self.warnings
                .add(format!("{location}: `#include` without a file name"));
            return;
        }
        if depth >= MAX_INCLUDE_DEPTH {
            self.warnings.add(format!(
                "{location}: includes nested too deeply at `{name}`"
            ));
            return;
        }
        let candidates = [self.root_dir.join(&name), dir.join(&name)];
        let Some(path) = candidates.iter().find(|p| p.is_file()) else {
            self.warnings
                .add(format!("{location}: included file `{name}` not found"));
            return;
        };
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if self.open_files.contains(&canonical) {
            self.warnings
                .add(format!("{location}: `{name}` includes itself; skipped"));
            return;
        }
        match read_text(path) {
            Ok(text) => {
                self.open_files.push(canonical);
                self.text(&text, path, depth + 1);
                self.open_files.pop();
            }
            Err(error) => self
                .warnings
                .add(format!("{location}: couldn't read `{name}`: {error}")),
        }
    }

    /// Headers and opcodes on one line, comments already gone.
    fn line(&mut self, line: &str, location: Location) {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if bytes[i] == b'<' {
                let Some(close) = line[i..].find('>') else {
                    self.warnings
                        .add(format!("{location}: header without a closing `>`"));
                    return;
                };
                let name = line[i + 1..i + close].trim().to_ascii_lowercase();
                self.header(&name, location);
                i += close + 1;
                continue;
            }
            let Some(equals) = line[i..].find('=').map(|e| i + e) else {
                self.warnings
                    .add(format!("{location}: ignored `{}`", line[i..].trim()));
                return;
            };
            let written = line[i..equals].trim();
            // Stray words before an opcode: keep the opcode, warn about the rest.
            let name = written.rsplit(char::is_whitespace).next().unwrap_or("");
            if name.len() != written.len() {
                self.warnings.add(format!(
                    "{location}: ignored `{}`",
                    written[..written.len() - name.len()].trim()
                ));
            }
            let end = value_end(line, equals + 1);
            let value = line[equals + 1..end].trim();
            if name.is_empty() || name.contains('<') {
                self.warnings
                    .add(format!("{location}: ignored `={value}` without a name"));
            } else {
                self.opcode(name, value, location);
            }
            i = end;
        }
    }

    fn header(&mut self, name: &str, location: Location) {
        self.finish_region();
        self.section = match name {
            "control" => Section::Control,
            "global" => {
                self.global.clear();
                self.master.clear();
                self.group.clear();
                Section::Global
            }
            "master" => {
                self.master.clear();
                self.group.clear();
                Section::Master
            }
            "group" => {
                self.group.clear();
                Section::Group
            }
            "region" => {
                self.region = Some((Vec::new(), location.to_string()));
                Section::Region
            }
            _ => {
                self.warnings.unsupported(&format!("<{name}>"));
                Section::Ignored
            }
        };
    }

    fn opcode(&mut self, name: &str, value: &str, location: Location) {
        // Some instruments set this outside `<control>`; it means the same anywhere.
        if name == "default_path" {
            self.default_path = value.replace('\\', "/");
            return;
        }
        let opcode = Opcode {
            name: name.to_owned(),
            value: value.to_owned(),
        };
        match self.section {
            Section::Start | Section::Global => self.global.push(opcode),
            Section::Master => self.master.push(opcode),
            Section::Group => self.group.push(opcode),
            Section::Region => {
                if let Some((opcodes, _)) = &mut self.region {
                    opcodes.push(opcode);
                }
            }
            Section::Control => self.control(name, value, location),
            Section::Ignored => {}
        }
    }

    fn control(&mut self, name: &str, value: &str, location: Location) {
        let number = || value.split_whitespace().next()?.parse::<i32>().ok();
        match name {
            "note_offset" | "octave_offset" => match number() {
                Some(offset) if name == "note_offset" => self.note_offset = offset.clamp(-127, 127),
                Some(offset) => self.octave_offset = offset.clamp(-10, 10),
                None => self.warnings.bad_value(name, value, &location.to_string()),
            },
            _ if is_ignored(name) => {}
            _ => self.warnings.unsupported(name),
        }
    }

    fn finish_region(&mut self) {
        let Some((own, location)) = self.region.take() else {
            return;
        };
        let mut opcodes = Vec::with_capacity(
            self.global.len() + self.master.len() + self.group.len() + own.len(),
        );
        opcodes.extend_from_slice(&self.global);
        opcodes.extend_from_slice(&self.master);
        opcodes.extend_from_slice(&self.group);
        opcodes.extend(own);
        self.regions.push(RawRegion {
            opcodes,
            default_path: self.default_path.clone(),
            note_offset: self.note_offset + 12 * self.octave_offset,
            location,
        });
    }
}

/// Where the value starting at `start` ends: at the next header, or before
/// whitespace followed by the next `name=`. Values can hold spaces, like
/// sample names.
fn value_end(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let is_name = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut k = start;
    while k < bytes.len() {
        if bytes[k] == b'<' {
            return k;
        }
        if !bytes[k].is_ascii_whitespace() {
            k += 1;
            continue;
        }
        let mut word = k;
        while word < bytes.len() && bytes[word].is_ascii_whitespace() {
            word += 1;
        }
        let mut after = word;
        while after < bytes.len() && is_name(bytes[after]) {
            after += 1;
        }
        if word < bytes.len() && bytes[word] == b'<' {
            return k;
        }
        if after > word && after < bytes.len() && bytes[after] == b'=' {
            return k;
        }
        k = word;
    }
    bytes.len()
}

/// Removes `// line` and `/* block */` comments, keeping line breaks so
/// warnings point at the right line.
fn strip_comments(text: &str) -> Cow<'_, str> {
    if !text.contains("//") && !text.contains("/*") {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'/' || !matches!(bytes[i + 1], b'/' | b'*') {
            i += 1;
            continue;
        }
        out.push_str(&text[kept..i]);
        let end = if bytes[i + 1] == b'/' {
            text[i..].find('\n').map_or(text.len(), |n| i + n)
        } else {
            out.push(' ');
            let end = text[i + 2..]
                .find("*/")
                .map_or(text.len(), |n| i + 2 + n + 2);
            let newlines = text[i..end].matches('\n').count();
            out.extend(std::iter::repeat_n('\n', newlines));
            end
        };
        kept = end;
        i = end;
    }
    out.push_str(&text[kept..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions(text: &str) -> (Vec<RawRegion>, Vec<String>) {
        let mut warnings = Warnings::default();
        let regions = parse(text, Path::new("/nonexistent/test.sfz"), &mut warnings);
        (regions, warnings.finish())
    }

    fn value<'a>(region: &'a RawRegion, name: &str) -> Option<&'a str> {
        region
            .opcodes
            .iter()
            .rev()
            .find(|o| o.name == name)
            .map(|o| o.value.as_str())
    }

    #[test]
    fn values_run_to_the_next_opcode_and_keep_spaces() {
        let (regions, warnings) =
            regions("<region> sample=Grand Piano C4 v1.wav lokey=60   hikey=62<region>key=1");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(regions.len(), 2);
        assert_eq!(value(&regions[0], "sample"), Some("Grand Piano C4 v1.wav"));
        assert_eq!(value(&regions[0], "lokey"), Some("60"));
        assert_eq!(value(&regions[0], "hikey"), Some("62"));
        assert_eq!(value(&regions[1], "key"), Some("1"));
    }

    #[test]
    fn comments_go_and_line_numbers_stay() {
        let text = "// header\n<region> sample=a.wav // trailing\n/* block\nover lines */ <region> sample=b.wav\n<region> bogus";
        let (regions, warnings) = regions(text);
        assert_eq!(regions.len(), 3);
        assert_eq!(value(&regions[0], "sample"), Some("a.wav"));
        assert_eq!(regions[1].location, "test.sfz:4");
        assert_eq!(value(&regions[1], "sample"), Some("b.wav"));
        assert_eq!(warnings, ["test.sfz:5: ignored `bogus`"]);
    }

    #[test]
    fn headers_inherit_and_reset() {
        let text = "<global> volume=-3 <master> pan=10 <group> lokey=1 <region> sample=a.wav
            <group> hikey=5 <region> sample=b.wav <master> <region> sample=c.wav";
        let (regions, _) = regions(text);
        let names = |r: &RawRegion| r.opcodes.iter().map(|o| o.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&regions[0]), ["volume", "pan", "lokey", "sample"]);
        assert_eq!(names(&regions[1]), ["volume", "pan", "hikey", "sample"]);
        assert_eq!(names(&regions[2]), ["volume", "sample"]);
    }

    #[test]
    fn defines_substitute_longest_name_first() {
        let text =
            "#define $KEY 60\n#define $KEYS 70\n<region> lokey=$KEY hikey=$KEYS sample=$KEY.wav";
        let (regions, _) = regions(text);
        assert_eq!(value(&regions[0], "lokey"), Some("60"));
        assert_eq!(value(&regions[0], "hikey"), Some("70"));
        assert_eq!(value(&regions[0], "sample"), Some("60.wav"));
    }

    #[test]
    fn control_sets_paths_and_offsets() {
        let text = "<control> default_path=Samples\\Piano\\ note_offset=2 octave_offset=-1 hint_ram_based=1
            <region> sample=a.wav";
        let (regions, warnings) = regions(text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(regions[0].default_path, "Samples/Piano/");
        assert_eq!(regions[0].note_offset, -10);
    }

    #[test]
    fn unknown_headers_are_reported_and_skipped() {
        let (regions, warnings) = regions("<curve> curve_index=7 v000=0 <region> sample=a.wav");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].opcodes.len(), 1);
        assert_eq!(warnings, ["Unsupported, ignored: `<curve>`"]);
    }

    #[test]
    fn missing_includes_warn() {
        let (_, warnings) = regions("#include \"nowhere.sfz\"\n<region> sample=a.wav");
        assert_eq!(
            warnings,
            ["test.sfz:1: included file `nowhere.sfz` not found"]
        );
    }
}
