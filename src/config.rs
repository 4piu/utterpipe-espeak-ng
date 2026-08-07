use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_wpm: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplitude: Option<u8>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("rate_wpm must be from 80 through 450")]
    Rate,
    #[error("pitch must be from 0 through 100")]
    Pitch,
    #[error("amplitude must be from 0 through 200")]
    Amplitude,
}

impl ProviderOptions {
    /// Validate all provider-specific engine controls.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when an option is outside its supported range.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self
            .rate_wpm
            .is_some_and(|value| !(80..=450).contains(&value))
        {
            return Err(ConfigError::Rate);
        }
        if self.pitch.is_some_and(|value| value > 100) {
            return Err(ConfigError::Pitch);
        }
        if self.amplitude.is_some_and(|value| value > 200) {
            return Err(ConfigError::Amplitude);
        }
        Ok(())
    }
}

#[must_use]
pub fn options_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "rate_wpm": {"type": "integer", "minimum": 80, "maximum": 450},
            "pitch": {"type": "integer", "minimum": 0, "maximum": 100},
            "amplitude": {"type": "integer", "minimum": 0, "maximum": 200}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_strict() {
        assert!(serde_json::from_value::<ProviderOptions>(json!({"unknown": 1})).is_err());
        let options = ProviderOptions {
            rate_wpm: Some(180),
            pitch: Some(40),
            amplitude: Some(120),
        };
        assert!(options.validate().is_ok());
    }

    #[test]
    fn option_bounds_are_enforced() {
        let mut options = ProviderOptions {
            rate_wpm: Some(79),
            ..ProviderOptions::default()
        };
        assert!(matches!(options.validate(), Err(ConfigError::Rate)));
        options.rate_wpm = Some(450);
        options.pitch = Some(101);
        assert!(matches!(options.validate(), Err(ConfigError::Pitch)));
        options.pitch = Some(100);
        options.amplitude = Some(201);
        assert!(matches!(options.validate(), Err(ConfigError::Amplitude)));
    }
}
