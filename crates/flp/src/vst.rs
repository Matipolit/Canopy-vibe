//! What third-party plugins keep in FL's wrapper state, for the ones the
//! importer carries settings over from.
//!
//! FL wraps a VST's own state in records (see [`records`]); record
//! [`STATE`] is the plugin's. A VST 2 plugin's is its chunk as is; a VST 3
//! plugin's is records again, the component's state in record 3. What's
//! inside is the plugin's business: TDR's and Valhalla's are XML,
//! Kilohearts' a zip holding `state.json`, Xfer's OTT its parameters as
//! floats.

use serde_json::{Map, Value};

/// The wrapper's record holding the plugin's own state.
pub const STATE: u32 = 53;
/// In a VST 3 plugin's state, the record holding its component's.
const VST3_COMPONENT: u32 = 3;

/// The records of FL's wrapper state: after a 32-bit kind, each a 32-bit
/// id, a 64-bit length and that many bytes.
pub fn records(data: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut at = 4;
    std::iter::from_fn(move || {
        let id = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?);
        let len = u64::from_le_bytes(data.get(at + 4..at + 12)?.try_into().ok()?);
        let start = at + 12;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        let bytes = data.get(start..end)?;
        at = end;
        Some((id, bytes))
    })
}

/// The plugin's own state: a VST 2 plugin's chunk, or a VST 3 plugin's
/// component state.
pub fn chunk(state: &[u8]) -> Option<&[u8]> {
    let (_, own) = records(state).find(|(id, _)| *id == STATE)?;
    // A VST 3's starts with a 64-byte record 1.
    let vst3 = own.len() >= 16
        && own[..4] == 1u32.to_le_bytes()
        && own[4..8] == 1u32.to_le_bytes()
        && own[8..16] == 64u64.to_le_bytes();
    if vst3 && let Some((_, component)) = records(own).find(|(id, _)| *id == VST3_COMPONENT) {
        return Some(component);
    }
    Some(own)
}

/// Whether `id` is one of canopy's own instruments as a CLAP plugin
/// (`canopy-clap` keeps each built-in's id as its CLAP id).
pub fn is_canopy(id: &crate::model::PluginId) -> bool {
    id.format == crate::model::PluginFormat::Clap && instruments::find_builtin(&id.id).is_some()
}

/// The settings of a canopy instrument FL played through canopy's own
/// CLAP plugin: the instrument's id and each parameter's value in its own
/// units, by parameter id, as `canopy-clap` saves them (JSON:
/// `{"instrument", "version", "params"}`). `None` for any other plugin.
pub fn canopy_state(
    plugin: &crate::model::Plugin,
) -> Option<(String, std::collections::BTreeMap<String, f32>)> {
    let id = plugin.vst.as_ref()?.plugin.as_ref()?;
    if !is_canopy(id) {
        return None;
    }
    let state: Value = serde_json::from_slice(&hosted_state(plugin)?).ok()?;
    if state.get("instrument")?.as_str()? != id.id {
        return None;
    }
    let params = state
        .get("params")?
        .as_object()?
        .iter()
        .filter_map(|(param, value)| Some((param.clone(), value.as_f64()? as f32)))
        .collect();
    Some((id.id.clone(), params))
}

/// The plugin's state as canopy's plugin host loads it
/// (`plugins::state`), from FL's wrapper state, for importing the plugin as
/// itself. `None` if the wrapper's isn't laid out as expected.
///
/// - CLAP: four bytes of FL's, then the plugin's own state.
/// - VST3: records after a 32-bit kind: 1, 64 bytes of FL's; 3 (or, in
///   some, only 2), the component's state; 4, the controller's.
/// - VST2: a 21-byte header (-9 as `i32`, a flags byte whose bit 3 means
///   a chunk follows, -2, the chunk's length as `u32`, a `u32`, and the
///   current program as `u32`), then the bank chunk, or else a parameter
///   count (`u32`) and each parameter's value (`f32`).
pub fn hosted_state(plugin: &crate::model::Plugin) -> Option<Vec<u8>> {
    use crate::model::PluginFormat;
    use plugins::state::{self as host, Vst2Data};
    let id = plugin.vst.as_ref()?.plugin.as_ref()?;
    let (_, own) = records(&plugin.state).find(|(id, _)| *id == STATE)?;
    match id.format {
        PluginFormat::Clap => own.get(4..).map(<[u8]>::to_vec),
        PluginFormat::Vst3 => {
            let parts: Vec<(u32, &[u8])> = records(own).collect();
            let part = |id: u32| parts.iter().find(|(i, _)| *i == id).map(|(_, b)| *b);
            let component = part(VST3_COMPONENT).or_else(|| part(2))?;
            Some(host::vst3(component, part(4).unwrap_or_default()))
        }
        PluginFormat::Vst2 => {
            let unique_id = i32::from_str_radix(&id.id, 16)
                .ok()
                .or_else(|| u32::from_str_radix(&id.id, 16).ok().map(|v| v as i32))?;
            let header = own.get(..21)?;
            let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
            if word(0) as i32 != -9 || word(5) as i32 != -2 {
                return None;
            }
            let (flags, length, program) = (header[4], word(9) as usize, word(17) as i32);
            let rest = &own[21..];
            let data = if flags & 0x08 != 0 {
                Vst2Data::Chunk(rest.get(..length)?)
            } else {
                let count = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
                let values = rest.get(4..4 + count.checked_mul(4)?)?;
                Vst2Data::Params(
                    values
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect(),
                )
            };
            Some(host::vst2(unique_id, program, &data))
        }
    }
}

/// The attributes of the first `element` in an XML state, in order, with
/// entities decoded.
pub fn xml_attributes(state: &[u8], element: &str) -> Option<Vec<(String, String)>> {
    let open = format!("<{element}");
    let start = find(state, open.as_bytes())? + open.len();
    let rest = &state[start..];
    // A space must follow the name: `<logical ` is not `<logicalFoo `.
    if !rest.first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let end = rest.iter().position(|b| *b == b'>')?;
    let text = String::from_utf8_lossy(&rest[..end]);
    let mut attributes = Vec::new();
    let mut text = text.as_ref();
    while let Some(eq) = text.find("=\"") {
        let name = text[..eq].trim().to_owned();
        let value_start = eq + 2;
        let Some(len) = text[value_start..].find('"') else {
            break;
        };
        let value = &text[value_start..value_start + len];
        attributes.push((name, unescape(value)));
        text = &text[value_start + len + 1..];
    }
    Some(attributes)
}

fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The numeric value of attribute `name`.
pub fn attribute(attributes: &[(String, String)], name: &str) -> Option<f32> {
    attributes
        .iter()
        .find(|(n, _)| n == name)?
        .1
        .trim()
        .parse()
        .ok()
}

/// Attribute `name`'s text.
pub fn text<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    Some(attributes.iter().find(|(n, _)| n == name)?.1.as_str())
}

/// The most a MeldaProduction plugin's settings may inflate to: theirs
/// are a few hundred bytes.
const MAX_MELDA: usize = 1 << 20;

/// A MeldaProduction plugin's settings: its state keeps its XML
/// zlib-compressed (once for each of its settings slots; the first is the
/// one in use), `<{name}settings #param="value" ...>`, writing only the
/// parameters away from their defaults, in the plugin's own units. Newer
/// versions write the same as binary XML. `name` is the plugin's
/// (`MSaturator`). The root element's attributes; [`melda_elements`] has
/// the rest.
pub fn melda(state: &[u8], name: &str) -> Option<Vec<(String, String)>> {
    melda_elements(state, name)?.into_iter().next().map(|(_, a)| a)
}

/// Every element of a MeldaProduction plugin's settings, in order, the
/// root first: its name and attributes (as text either way).
pub fn melda_elements(state: &[u8], name: &str) -> Option<Vec<(String, Vec<(String, String)>)>> {
    let root = format!("{name}settings");
    let mut from = 0;
    while let Some(at) = find(&state[from..], b"x\xda") {
        let start = from + at;
        from = start + 2;
        let Ok(xml) =
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&state[start..], MAX_MELDA)
        else {
            continue;
        };
        let elements = melda_binary(&xml).unwrap_or_else(|| text_elements(&xml));
        if elements.first().is_some_and(|(n, _)| *n == root) {
            return Some(elements);
        }
    }
    None
}

/// The elements of text XML, in order: each `<name attributes>` (closing
/// tags and the declaration left out).
fn text_elements(xml: &[u8]) -> Vec<(String, Vec<(String, String)>)> {
    let text = String::from_utf8_lossy(xml);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let tag = rest[..close].trim_end_matches('/');
        rest = &rest[close + 1..];
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let mut attributes = Vec::new();
        let mut attrs = &tag[name_end..];
        while let Some(eq) = attrs.find("=\"") {
            let key = attrs[..eq].trim().to_owned();
            let value_start = eq + 2;
            let Some(len) = attrs[value_start..].find('"') else { break };
            attributes.push((key, attrs[value_start..value_start + len].to_owned()));
            attrs = &attrs[value_start + len + 1..];
        }
        out.push((tag[..name_end].to_owned(), attributes));
    }
    out
}

/// Melda's binary XML, its elements in order, attributes as text: after
/// `\0MBX`, `X` and a NUL-terminated name open an element and `/` closes
/// one; `A`, a NUL-terminated name and a type give an attribute: `d` an
/// f64, `f` an f32, `s` a NUL-terminated string, `1`, `2`, `4` or `8` a
/// signed integer of that many bytes, all little-endian.
pub fn melda_binary(data: &[u8]) -> Option<Vec<(String, Vec<(String, String)>)>> {
    let rest = data.strip_prefix(b"\0MBX")?;
    let name = |at: usize| -> Option<(String, usize)> {
        let len = rest.get(at..)?.iter().position(|b| *b == 0)?;
        Some((String::from_utf8_lossy(&rest[at..at + len]).into_owned(), at + len + 1))
    };
    let mut elements: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut at = 0;
    while let Some(&tag) = rest.get(at) {
        match tag {
            b'X' => {
                let (element, next) = name(at + 1)?;
                elements.push((element, Vec::new()));
                at = next;
            }
            b'A' => {
                let (key, next) = name(at + 1)?;
                let kind = *rest.get(next)?;
                let bytes = |n: usize| rest.get(next + 1..next + 1 + n);
                let (value, used) = match kind {
                    b'd' => (f64::from_le_bytes(bytes(8)?.try_into().ok()?).to_string(), 8),
                    b'f' => (f32::from_le_bytes(bytes(4)?.try_into().ok()?).to_string(), 4),
                    b'1' => ((bytes(1)?[0] as i8).to_string(), 1),
                    b'2' => (i16::from_le_bytes(bytes(2)?.try_into().ok()?).to_string(), 2),
                    b'4' => (i32::from_le_bytes(bytes(4)?.try_into().ok()?).to_string(), 4),
                    b'8' => (i64::from_le_bytes(bytes(8)?.try_into().ok()?).to_string(), 8),
                    b's' => {
                        let (text, end) = name(next + 1)?;
                        (text, end - next - 1)
                    }
                    _ => return None,
                };
                elements.last_mut()?.1.push((key, value));
                at = next + 1 + used;
            }
            b'/' => at += 1,
            _ => break,
        }
    }
    (!elements.is_empty()).then_some(elements)
}

/// JUCE plugins' `<PARAM id="name" value="v"/>` settings, as numbers.
pub fn juce_params(state: &[u8]) -> Vec<(String, f32)> {
    let text = String::from_utf8_lossy(state);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(at) = rest.find("<PARAM id=\"") {
        rest = &rest[at + 11..];
        let Some(end) = rest.find('"') else { break };
        let id = rest[..end].to_owned();
        rest = &rest[end..];
        let Some(value_at) = rest.find("value=\"") else { break };
        let value_text = &rest[value_at + 7..];
        let Some(value_end) = value_text.find('"') else { break };
        if let Ok(value) = value_text[..value_end].trim().parse() {
            out.push((id, value));
        }
        rest = &value_text[value_end..];
    }
    out
}

/// The most a zipped `state.json` may grow to: Kilohearts' are tens of
/// kilobytes.
const MAX_JSON: usize = 4 << 20;

/// A Kilohearts plugin's settings: the `model` of the `state.json` zipped
/// into its state. Its numbers are strings.
pub fn kilohearts(state: &[u8]) -> Option<Map<String, Value>> {
    const LOCAL_HEADER: &[u8] = b"PK\x03\x04";
    let mut from = 0;
    while let Some(at) = find(&state[from..], LOCAL_HEADER) {
        let header = from + at;
        from = header + LOCAL_HEADER.len();
        let field = |offset: usize| -> Option<u16> {
            let at = header + offset;
            Some(u16::from_le_bytes(state.get(at..at + 2)?.try_into().ok()?))
        };
        let (Some(method), Some(name_len), Some(extra_len)) = (field(8), field(26), field(28))
        else {
            continue;
        };
        let name_start = header + 30;
        let data_start = name_start + usize::from(name_len) + usize::from(extra_len);
        if state.get(name_start..name_start + usize::from(name_len)) != Some(b"state.json") {
            continue;
        }
        let data = state.get(data_start..)?;
        // Deflated, its sizes in a descriptor after the data: the stream
        // ends itself.
        let json = match method {
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_JSON).ok()?,
            0 => data.to_vec(),
            _ => return None,
        };
        let mut values = serde_json::Deserializer::from_slice(&json).into_iter::<Value>();
        if let Some(Ok(Value::Object(mut object))) = values.next()
            && let Some(Value::Object(model)) = object.remove("model")
        {
            return Some(model);
        }
    }
    None
}

/// A number from a Kilohearts model, where they're kept as strings.
pub fn number(model: &Map<String, Value>, key: &str) -> Option<f32> {
    match model.get(key)? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_f64().map(|n| n as f32),
        _ => None,
    }
}

/// How many parameters Xfer's OTT has.
pub const OTT_PARAMS: usize = 19;

/// OTT's parameters, each `0..=1` as a host sees them: depth, time, in
/// gain, out gain, Clean XOV, the low, mid and high thresholds, the low,
/// mid and high input gains, the bypasses of upward compression (low, mid,
/// high) and of downward, then Upwd% and Dnwd%. Its VST 3 keeps them
/// first in its state; its VST 2 after their count, a little way in.
pub fn ott(state: &[u8]) -> Option<[f32; OTT_PARAMS]> {
    let chunk = chunk(state)?;
    let read = |at: usize| -> Option<[f32; OTT_PARAMS]> {
        let mut params = [0.0; OTT_PARAMS];
        for (k, p) in params.iter_mut().enumerate() {
            let at = at + 4 * k;
            *p = f32::from_le_bytes(chunk.get(at..at + 4)?.try_into().ok()?);
        }
        params
            .iter()
            .all(|p| (0.0..=1.0).contains(p))
            .then_some(params)
    };
    if let Some(params) = read(0) {
        return Some(params);
    }
    let count = (OTT_PARAMS as u32).to_le_bytes();
    (0..chunk.len().min(256))
        .filter(|at| chunk[*at..].starts_with(&count))
        .find_map(|at| read(at + 4))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32, data: &[u8]) -> Vec<u8> {
        let mut out = id.to_le_bytes().to_vec();
        out.extend((data.len() as u64).to_le_bytes());
        out.extend(data);
        out
    }

    /// A wrapper state around the plugin's own `own`.
    fn wrapper(own: &[u8]) -> Vec<u8> {
        let mut state = 12u32.to_le_bytes().to_vec();
        state.extend(record(54, b"Plugin"));
        state.extend(record(56, b"Vendor"));
        state.extend(record(STATE, own));
        state
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    const OTT_VALUES: [f32; OTT_PARAMS] = [
        0.78, 0.15, 0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.58, 0.45, 0.67, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.5, 0.5,
    ];

    #[test]
    fn reads_vst2_chunks_and_vst3_components() {
        let state = wrapper(b"chunk");
        assert_eq!(chunk(&state), Some(&b"chunk"[..]));

        let mut vst3 = 1u32.to_le_bytes().to_vec();
        vst3.extend(record(1, &[0; 64]));
        vst3.extend(record(VST3_COMPONENT, b"component"));
        vst3.extend(record(4, b"controller"));
        assert_eq!(chunk(&wrapper(&vst3)), Some(&b"component"[..]));
        // Cut short: no records past the end.
        assert_eq!(records(&state[..state.len() - 1]).count(), 2);
    }

    #[test]
    fn reads_ott_from_either_kind() {
        let mut vst3 = 1u32.to_le_bytes().to_vec();
        vst3.extend(record(1, &[0; 64]));
        vst3.extend(record(VST3_COMPONENT, &floats(&OTT_VALUES)));
        assert_eq!(ott(&wrapper(&vst3)), Some(OTT_VALUES));

        // As OTT's VST 2 saves it: a header, the count at an odd offset,
        // the values, then the program's name.
        let mut vst2 = vec![
            0xF7, 0xFF, 0xFF, 0xFF, 0x05, 0xFE, 0xFF, 0xFF, 0xFF, 0, 0, 0,
        ];
        vst2.extend([0; 9]);
        vst2.extend(19u32.to_le_bytes());
        vst2.extend(floats(&OTT_VALUES));
        vst2.extend(b"\x01\0\0\0DefaultProgram  ");
        assert_eq!(ott(&wrapper(&vst2)), Some(OTT_VALUES));
        assert_eq!(ott(&wrapper(b"nothing like it")), None);
    }

    #[test]
    fn reads_xml_attributes() {
        let xml = br#"junk<?xml version="1.0"?> <TDRKotelnikov thresholdParam="-25.0" peakCrestParam="Peak"><logical thresholdParam="-30.8" name="a &amp; b" ratioParam="2.2"/>"#;
        let root = xml_attributes(xml, "TDRKotelnikov").unwrap();
        assert_eq!(text(&root, "peakCrestParam"), Some("Peak"));
        let logical = xml_attributes(xml, "logical").unwrap();
        assert_eq!(attribute(&logical, "thresholdParam"), Some(-30.8));
        assert_eq!(attribute(&logical, "ratioParam"), Some(2.2));
        assert_eq!(text(&logical, "name"), Some("a & b"));
        assert!(xml_attributes(xml, "missing").is_none());
    }

    #[test]
    fn reads_kilohearts_zipped_json() {
        let json = br#"{"version":{"format":"1"},"model":{"threshold":"0.5","mix":"1","$type":"compactor"}}"#;
        let deflated = miniz_oxide::deflate::compress_to_vec(json, 6);
        // A zip's local header: version, flags (sizes in a descriptor),
        // method 8, time, date, crc, sizes, name and extra lengths.
        let mut zip = b"PK\x03\x04".to_vec();
        zip.extend([20, 0, 8, 0, 8, 0, 0, 0, 0, 0]);
        zip.extend([0; 12]);
        zip.extend(10u16.to_le_bytes());
        zip.extend(0u16.to_le_bytes());
        zip.extend(b"state.json");
        zip.extend(&deflated);
        zip.extend(b"PK\x07\x08 and the rest of the zip");
        let model = kilohearts(&wrapper(&zip)).unwrap();
        assert_eq!(number(&model, "threshold"), Some(0.5));
        assert_eq!(number(&model, "mix"), Some(1.0));
        assert!(kilohearts(b"no zip here").is_none());
    }
}
