//! The bytes [`crate::PluginInstance::save_state`] makes and `load_state`
//! takes, and the ids [`crate::PluginInfo`] names plugins by, for code that
//! makes them from somewhere else (a plugin's state in an FL Studio
//! project) rather than from a running plugin. The backends use the same
//! functions, so there's one definition of each.
//!
//! All numbers are little-endian.
//!
//! - **CLAP:** the plugin's own state stream, as it is.
//! - **VST3:** [`VST3_MAGIC`], a version (`u32`, 1), then the component's
//!   state and the controller's, each a length (`u64`) and the bytes.
//! - **VST2:** [`VST2_MAGIC`], a version (`u32`, 1), the plugin's unique id
//!   (`i32`), its program (`i32`), then a kind (`u32`): 0, the bank chunk's
//!   length (`u64`) and the chunk; or 1, a parameter count (`u32`) and each
//!   parameter's value (`f32`), for plugins without chunks.

/// What a VST3 state starts with.
pub const VST3_MAGIC: &[u8; 8] = b"CNPYVST3";
/// What a VST2 state starts with.
pub const VST2_MAGIC: &[u8; 8] = b"CNPYVST2";
const VERSION: u32 = 1;
const VST2_CHUNK: u32 = 0;
const VST2_PARAMS: u32 = 1;

/// A VST3 plugin's state from its component's and controller's.
pub fn vst3(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut state = Vec::with_capacity(28 + component.len() + controller.len());
    state.extend_from_slice(VST3_MAGIC);
    state.extend_from_slice(&VERSION.to_le_bytes());
    for part in [component, controller] {
        state.extend_from_slice(&(part.len() as u64).to_le_bytes());
        state.extend_from_slice(part);
    }
    state
}

/// The component's and controller's states in a [`vst3`] state.
pub(crate) fn parse_vst3(state: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut rest = state.strip_prefix(VST3_MAGIC.as_slice())?;
    if take_u32(&mut rest)? != VERSION {
        return None;
    }
    let component = take_part(&mut rest)?;
    let controller = take_part(&mut rest)?;
    Some((component, controller))
}

/// What a VST2 state holds besides the plugin's id and program.
#[derive(Debug, Clone, PartialEq)]
pub enum Vst2Data<'a> {
    /// The plugin's bank chunk (`effGetChunk`).
    Chunk(&'a [u8]),
    /// Every parameter's value, in order, for a plugin without chunks.
    Params(Vec<f32>),
}

/// A VST2 plugin's state.
pub fn vst2(unique_id: i32, program: i32, data: &Vst2Data) -> Vec<u8> {
    let mut state = Vec::new();
    state.extend_from_slice(VST2_MAGIC);
    state.extend_from_slice(&VERSION.to_le_bytes());
    state.extend_from_slice(&unique_id.to_le_bytes());
    state.extend_from_slice(&program.max(0).to_le_bytes());
    match data {
        Vst2Data::Chunk(chunk) => {
            state.extend_from_slice(&VST2_CHUNK.to_le_bytes());
            state.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
            state.extend_from_slice(chunk);
        }
        Vst2Data::Params(values) => {
            state.extend_from_slice(&VST2_PARAMS.to_le_bytes());
            state.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                state.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    state
}

/// The unique id, program and data of a [`vst2`] state.
pub(crate) fn parse_vst2(state: &[u8]) -> Option<(i32, i32, Vst2Data<'_>)> {
    let mut rest = state.strip_prefix(VST2_MAGIC.as_slice())?;
    if take_u32(&mut rest)? != VERSION {
        return None;
    }
    let unique_id = take_u32(&mut rest)? as i32;
    let program = take_u32(&mut rest)? as i32;
    let data = match take_u32(&mut rest)? {
        VST2_CHUNK => Vst2Data::Chunk(take_part(&mut rest)?),
        VST2_PARAMS => {
            let count = take_u32(&mut rest)? as usize;
            let bytes = take(&mut rest, count.checked_mul(4)?)?;
            Vst2Data::Params(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect(),
            )
        }
        _ => return None,
    };
    Some((unique_id, program, data))
}

/// A VST2 plugin's id as [`crate::PluginInfo::id`] has it: its unique id
/// as 8 hex digits of its bits ("56697461" for `'Vita'`).
pub fn vst2_id(unique_id: i32) -> String {
    format!("{:08X}", unique_id as u32)
}

/// A VST3 class id as [`crate::PluginInfo::id`] has it, from its 16 bytes
/// as they are in memory: on Windows (`com`, COM's byte order, which swaps
/// the bytes of the first word and of each half of the second), or
/// elsewhere. The result is the same for a plugin on every system: 32 hex
/// digits of the four words the class was declared with, as Steinberg
/// prints FUIDs.
pub fn vst3_id(bytes: &[u8; 16], com: bool) -> String {
    let b = bytes;
    let words = if com {
        [
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]
    } else {
        *b
    };
    words.iter().map(|b| format!("{b:02X}")).collect()
}

/// The inverse of [`vst3_id`].
pub fn vst3_bytes(id: &str, com: bool) -> Option<[u8; 16]> {
    if id.len() != 32 || !id.is_ascii() {
        return None;
    }
    let mut b = [0u8; 16];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[2 * i..2 * i + 2], 16).ok()?;
    }
    // The reordering is its own inverse.
    Some(if com {
        [
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]
    } else {
        b
    })
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, tail) = rest.split_at_checked(n)?;
    *rest = tail;
    Some(head)
}

fn take_u32(rest: &mut &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(take(rest, 4)?.try_into().ok()?))
}

fn take_part<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = u64::from_le_bytes(take(rest, 8)?.try_into().ok()?);
    take(rest, usize::try_from(len).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vst3_states_hold_both_halves() {
        let state = vst3(b"component", b"controller");
        assert_eq!(
            parse_vst3(&state),
            Some((&b"component"[..], &b"controller"[..]))
        );
        assert_eq!(parse_vst3(&state[..state.len() - 1]), None, "cut short");
        assert_eq!(parse_vst3(b"something else"), None);
    }

    #[test]
    fn vst2_states_hold_a_chunk_or_values() {
        let chunk = vst2(0x5669_7461, 3, &Vst2Data::Chunk(b"bank"));
        assert_eq!(
            parse_vst2(&chunk),
            Some((0x5669_7461, 3, Vst2Data::Chunk(&b"bank"[..])))
        );
        let values = vst2(7, 0, &Vst2Data::Params(vec![0.25, 1.0]));
        assert_eq!(
            parse_vst2(&values),
            Some((7, 0, Vst2Data::Params(vec![0.25, 1.0])))
        );
        assert_eq!(parse_vst2(&values[..values.len() - 2]), None);
    }

    #[test]
    fn ids_read_the_same_whatever_system_wrote_them() {
        assert_eq!(vst2_id(i32::from_be_bytes(*b"Vita")), "56697461");
        let declared = "6E33225254224A00AA69301AF318797D";
        let windows = vst3_bytes(declared, true).unwrap();
        assert_eq!(&windows[..4], &[0x52, 0x22, 0x33, 0x6E]);
        assert_eq!(vst3_id(&windows, true), declared);
        let elsewhere = vst3_bytes(declared, false).unwrap();
        assert_eq!(vst3_id(&elsewhere, false), declared);
        assert_eq!(vst3_bytes("nope", true), None);
    }
}
