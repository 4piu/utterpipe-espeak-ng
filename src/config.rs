use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::DEFAULT_VOICE_ID;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOptions {}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtteranceOptions {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub voice: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub rate_wpm: Option<u16>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub pitch: Option<u8>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub amplitude: Option<u8>,
}

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("voice must contain 1–256 code points and no Unicode control characters")]
    Voice,
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
        Ok(())
    }
}

impl UtteranceOptions {
    /// Validate controls supplied for one synthesis request.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when an option is outside its advertised schema.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.voice.as_ref().is_some_and(|voice| {
            !(1..=256).contains(&voice.chars().count()) || voice.chars().any(char::is_control)
        }) {
            return Err(ConfigError::Voice);
        }
        validate_controls(self.rate_wpm, self.pitch, self.amplitude)
    }

    #[must_use]
    pub fn resolved_voice(&self) -> &str {
        self.voice.as_deref().unwrap_or(DEFAULT_VOICE_ID)
    }
}

fn validate_controls(
    rate_wpm: Option<u16>,
    pitch: Option<u8>,
    amplitude: Option<u8>,
) -> Result<(), ConfigError> {
    if rate_wpm.is_some_and(|value| !(80..=450).contains(&value)) {
        return Err(ConfigError::Rate);
    }
    if pitch.is_some_and(|value| value > 100) {
        return Err(ConfigError::Pitch);
    }
    if amplitude.is_some_and(|value| value > 200) {
        return Err(ConfigError::Amplitude);
    }
    Ok(())
}

#[must_use]
pub fn provider_options_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {}
    })
}

#[must_use]
pub fn management_options_schema() -> Value {
    provider_options_schema()
}

#[must_use]
pub fn utterance_options_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "maxProperties": 64,
        "properties": {
            "voice": {
                "type": "string", "minLength": 1, "maxLength": 256,
                "title": "Voice",
                "description": "Selects an embedded eSpeak NG voice for this utterance.",
                "x-utterpipe": {
                    "default_behavior": "Omission uses the provider's default English voice.",
                    "use_when": "Use when another embedded language or voice should speak this utterance.",
                    "omit_when": "Omit when the default English voice is suitable."
                }
            },
            "rate_wpm": {
                "type": "integer", "minimum": 80, "maximum": 450,
                "title": "Speaking rate",
                "description": "Sets the eSpeak NG speaking rate in words per minute for this utterance.",
                "x-utterpipe": {
                    "default_behavior": "Omission uses the eSpeak NG default rate.",
                    "use_when": "Use when this utterance should be spoken faster or slower.",
                    "omit_when": "Omit when the default speaking rate is suitable.",
                    "unit": "words per minute"
                }
            },
            "pitch": {
                "type": "integer", "minimum": 0, "maximum": 100,
                "title": "Voice pitch",
                "description": "Sets the eSpeak NG pitch level for this utterance.",
                "x-utterpipe": {
                    "default_behavior": "Omission uses the eSpeak NG default pitch.",
                    "use_when": "Use when a higher or lower pitch helps convey the message.",
                    "omit_when": "Omit when the default pitch is suitable.",
                    "unit": "eSpeak NG pitch level"
                }
            },
            "amplitude": {
                "type": "integer", "minimum": 0, "maximum": 200,
                "title": "Voice amplitude",
                "description": "Sets the eSpeak NG synthesis amplitude for this utterance before host playback gain.",
                "x-utterpipe": {
                    "default_behavior": "Omission uses the eSpeak NG default amplitude.",
                    "use_when": "Use when the synthesized waveform should be quieter or louder.",
                    "omit_when": "Omit when host playback gain and the default amplitude are suitable.",
                    "unit": "eSpeak NG amplitude level"
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_strict_and_null_is_rejected() {
        assert!(serde_json::from_value::<ProviderOptions>(json!({"unknown": 1})).is_err());
        assert!(serde_json::from_value::<UtteranceOptions>(json!({"voice": null})).is_err());
        let options = UtteranceOptions {
            voice: Some("gmw/en".into()),
            rate_wpm: Some(180),
            pitch: Some(40),
            amplitude: Some(120),
        };
        assert!(options.validate().is_ok());
    }

    #[test]
    fn option_bounds_are_enforced() {
        let mut options = UtteranceOptions {
            voice: None,
            rate_wpm: Some(79),
            ..UtteranceOptions::default()
        };
        assert!(matches!(options.validate(), Err(ConfigError::Rate)));
        options.rate_wpm = Some(450);
        options.pitch = Some(101);
        assert!(matches!(options.validate(), Err(ConfigError::Pitch)));
        options.pitch = Some(100);
        options.amplitude = Some(201);
        assert!(matches!(options.validate(), Err(ConfigError::Amplitude)));
    }

    #[test]
    fn request_controls_and_voice_are_resolved_per_utterance() {
        let options = UtteranceOptions {
            voice: Some("default".into()),
            rate_wpm: Some(220),
            pitch: None,
            amplitude: Some(90),
        };
        assert_eq!(options.resolved_voice(), "default");
        assert_eq!(options.rate_wpm, Some(220));
        assert_eq!(options.amplitude, Some(90));
        assert_eq!(
            UtteranceOptions::default().resolved_voice(),
            DEFAULT_VOICE_ID
        );
    }
}
