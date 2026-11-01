//! The one SPA pod canopy hands PipeWire: the format its stream offers.
//! SPA's own pod builder is header-only C, so this writes the bytes as
//! `spa/pod/pod.h` lays them out: a pod is a size (of its body) and a type,
//! then the body, padded to 8 bytes; an object's body is its type and id,
//! then properties, each a key, flags and a pod.

const SPA_TYPE_ID: u32 = 3;
const SPA_TYPE_INT: u32 = 4;
const SPA_TYPE_ARRAY: u32 = 13;
const SPA_TYPE_OBJECT: u32 = 15;

const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
const SPA_PARAM_ENUM_FORMAT: u32 = 3;

const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
const SPA_FORMAT_AUDIO_FORMAT: u32 = 0x10001;
const SPA_FORMAT_AUDIO_RATE: u32 = 0x10003;
const SPA_FORMAT_AUDIO_CHANNELS: u32 = 0x10004;
const SPA_FORMAT_AUDIO_POSITION: u32 = 0x10005;

const SPA_MEDIA_TYPE_AUDIO: u32 = 1;
const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
/// 32-bit float, little-endian, interleaved: what the engine renders.
const SPA_AUDIO_FORMAT_F32_LE: u32 = 0x11b;
const SPA_AUDIO_CHANNEL_FL: u32 = 3;
const SPA_AUDIO_CHANNEL_FR: u32 = 4;

/// An 8-byte-aligned pod, as PipeWire reads it.
pub struct Pod(Vec<u64>);

impl Pod {
    pub fn as_ptr(&self) -> *const std::ffi::c_void {
        self.0.as_ptr().cast()
    }

    #[cfg(test)]
    fn bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

/// The EnumFormat param for interleaved stereo 32-bit float at `rate`.
pub fn stereo_f32_format(rate: u32) -> Pod {
    let mut props = Vec::new();
    prop(
        &mut props,
        SPA_FORMAT_MEDIA_TYPE,
        SPA_TYPE_ID,
        &[SPA_MEDIA_TYPE_AUDIO],
    );
    prop(
        &mut props,
        SPA_FORMAT_MEDIA_SUBTYPE,
        SPA_TYPE_ID,
        &[SPA_MEDIA_SUBTYPE_RAW],
    );
    prop(
        &mut props,
        SPA_FORMAT_AUDIO_FORMAT,
        SPA_TYPE_ID,
        &[SPA_AUDIO_FORMAT_F32_LE],
    );
    prop(&mut props, SPA_FORMAT_AUDIO_RATE, SPA_TYPE_INT, &[rate]);
    prop(&mut props, SPA_FORMAT_AUDIO_CHANNELS, SPA_TYPE_INT, &[2]);
    // An array of ids: the child pod's header (size of one, type), then the values.
    let positions = [SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR];
    let mut array = vec![4, SPA_TYPE_ID];
    array.extend(positions);
    prop(
        &mut props,
        SPA_FORMAT_AUDIO_POSITION,
        SPA_TYPE_ARRAY,
        &array,
    );

    let mut words = vec![
        0,
        SPA_TYPE_OBJECT,
        SPA_TYPE_OBJECT_FORMAT,
        SPA_PARAM_ENUM_FORMAT,
    ];
    words.extend(props);
    words[0] = ((words.len() - 2) * 4) as u32;
    let mut packed = Vec::with_capacity(words.len().div_ceil(2));
    for pair in words.chunks(2) {
        let high = pair.get(1).copied().unwrap_or(0);
        packed.push(u64::from(pair[0]) | (u64::from(high) << 32));
    }
    Pod(packed)
}

/// A property holding a pod of `kind` with `body` (in 32-bit words),
/// padded to 8 bytes.
fn prop(out: &mut Vec<u32>, key: u32, kind: u32, body: &[u32]) {
    out.extend([key, 0, (body.len() * 4) as u32, kind]);
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(pod: &Pod) -> Vec<u32> {
        pod.bytes()
            .chunks(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn the_format_is_laid_out_as_spa_builds_it() {
        let pod = stereo_f32_format(48_000);
        #[rustfmt::skip]
        let expected = [
            // Object: body size, type; object type and param id.
            160, 15, 0x40003, 3,
            // mediaType: key, flags, Id pod of 4 bytes, audio, padding.
            1, 0, 4, 3, 1, 0,
            // mediaSubtype: raw.
            2, 0, 4, 3, 1, 0,
            // audio.format: F32LE.
            0x10001, 0, 4, 3, 0x11b, 0,
            // audio.rate: Int.
            0x10003, 0, 4, 4, 48_000, 0,
            // audio.channels: Int.
            0x10004, 0, 4, 4, 2, 0,
            // audio.position: Array of 16 bytes: child (4 bytes, Id), FL, FR.
            0x10005, 0, 16, 13, 4, 3, 3, 4,
        ];
        assert_eq!(words(&pod), expected);
    }

    #[test]
    fn the_rate_is_the_one_asked_for() {
        assert_eq!(words(&stereo_f32_format(44_100))[26], 44_100);
    }
}
