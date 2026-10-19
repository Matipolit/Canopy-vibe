//! What the plugin saves in a host's project: JSON naming the instrument
//! and each parameter's value in the instrument's own units, by the
//! parameter's id (as canopy's own files do), so a state survives
//! parameters appended later. A parameter the state doesn't name loads at
//! its default; one it names that the instrument lacks is ignored.

use std::collections::BTreeMap;

use instruments::BuiltInInstrumentDescriptor;
use serde::{Deserialize, Serialize};

use crate::values::Values;

#[derive(Serialize, Deserialize)]
struct State {
    instrument: String,
    version: String,
    params: BTreeMap<String, f32>,
}

/// `values`, as the state of `instrument`.
pub fn save(instrument: &BuiltInInstrumentDescriptor, values: &Values) -> Vec<u8> {
    let params = instrument
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, param)| Some((param.id.to_owned(), values.plain(index)?)))
        .collect();
    let state = State {
        instrument: instrument.id.to_owned(),
        version: instrument.version.to_owned(),
        params,
    };
    serde_json::to_vec(&state).expect("a map of numbers serializes")
}

/// Sends every value `bytes` holds to `values`; an error if it isn't a
/// state of `instrument`.
pub fn load(
    instrument: &BuiltInInstrumentDescriptor,
    values: &Values,
    bytes: &[u8],
) -> Result<(), String> {
    let state: State = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if state.instrument != instrument.id {
        return Err(format!(
            "a state of {}, not {}",
            state.instrument, instrument.id
        ));
    }
    for (index, param) in instrument.params.iter().enumerate() {
        let plain = state
            .params
            .get(param.id)
            .copied()
            .unwrap_or_else(|| param.default_value());
        values.send_plain(index, plain);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guitar() -> &'static BuiltInInstrumentDescriptor {
        instruments::find_builtin("canopy.guitar").unwrap()
    }

    #[test]
    fn a_state_round_trips() {
        let values = Values::new(guitar().params);
        values.send(0, 0.25);
        values.send(3, 0.75);
        let saved = save(guitar(), &values);
        let loaded = Values::new(guitar().params);
        load(guitar(), &loaded, &saved).unwrap();
        for index in 0..values.len() {
            assert_eq!(loaded.plain(index), values.plain(index), "{index}");
        }
    }

    #[test]
    fn missing_parameters_load_at_their_defaults_and_unknown_ones_are_ignored() {
        let values = Values::new(guitar().params);
        values.send(0, 1.0);
        let json = br#"{"instrument":"canopy.guitar","version":"1.0","params":{"no.such":3}}"#;
        load(guitar(), &values, json).unwrap();
        assert_eq!(values.plain(0), Some(guitar().params[0].default_value()));
    }

    #[test]
    fn another_instruments_state_is_refused() {
        let wavetable = instruments::find_builtin("canopy.wavetable").unwrap();
        let saved = save(wavetable, &Values::new(wavetable.params));
        let values = Values::new(guitar().params);
        assert!(load(guitar(), &values, &saved).is_err());
        assert!(load(guitar(), &values, b"not json").is_err());
    }
}
