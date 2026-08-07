use std::{collections::HashSet, fmt, io::ErrorKind};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAGIC: &[u8; 4] = b"UTP1";
const HEADER_LENGTH: usize = 12;
const MAX_CONTROL_BYTES: usize = 1_048_576;
const MAX_AUDIO_BYTES: usize = 268_435_456;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Control = 0x01,
    Audio = 0x02,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn control(payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: FrameKind::Control,
            payload: payload.into(),
        }
    }

    pub fn audio(payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: FrameKind::Audio,
            payload: payload.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WireFailure {
    #[error("wire I/O failed")]
    Io,
    #[error("wire frame is truncated")]
    Truncated,
    #[error("wire frame header is invalid")]
    Header,
    #[error("wire frame length is invalid")]
    Length,
    #[error("control message is invalid")]
    Control,
}

#[derive(Debug)]
pub struct Request {
    pub id: String,
    pub method: String,
    pub params: Map<String, Value>,
}

impl Request {
    /// Decode a strict JSON request envelope.
    ///
    /// # Errors
    ///
    /// Returns [`WireFailure`] for malformed JSON, duplicate keys, or invalid
    /// required envelope fields.
    pub fn parse(bytes: &[u8]) -> Result<Self, WireFailure> {
        let value = decode_strict_json(bytes)?;
        let object = value.as_object().ok_or(WireFailure::Control)?;
        if object.get("kind").and_then(Value::as_str) != Some("request") {
            return Err(WireFailure::Control);
        }
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_request_id(value))
            .ok_or(WireFailure::Control)?
            .to_owned();
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty() && value.len() <= 128 && !value.contains(['\r', '\n', '\0'])
            })
            .ok_or(WireFailure::Control)?
            .to_owned();
        let params = object
            .get("params")
            .and_then(Value::as_object)
            .ok_or(WireFailure::Control)?
            .clone();
        Ok(Self { id, method, params })
    }
}

/// Read and validate one framed message, returning `None` at clean EOF.
///
/// # Errors
///
/// Returns [`WireFailure`] for I/O errors, truncated/invalid headers, or
/// payload lengths outside protocol bounds.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Frame>, WireFailure> {
    let mut header = [0_u8; HEADER_LENGTH];
    match reader.read(&mut header[..1]).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(_) => return Err(WireFailure::Io),
    }
    read_exact(reader, &mut header[1..]).await?;
    if &header[..4] != MAGIC || header[5..8] != [0, 0, 0] {
        return Err(WireFailure::Header);
    }
    let kind = match header[4] {
        0x01 => FrameKind::Control,
        0x02 => FrameKind::Audio,
        _ => return Err(WireFailure::Header),
    };
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    validate_length(kind, length)?;
    let mut payload = vec![0_u8; length];
    read_exact(reader, &mut payload).await?;
    Ok(Some(Frame { kind, payload }))
}

/// Write and flush one validated protocol frame.
///
/// # Errors
///
/// Returns [`WireFailure`] when the payload length is invalid or output fails.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), WireFailure> {
    validate_length(frame.kind, frame.payload.len())?;
    let length = u32::try_from(frame.payload.len()).map_err(|_| WireFailure::Length)?;
    let mut header = [0_u8; HEADER_LENGTH];
    header[..4].copy_from_slice(MAGIC);
    header[4] = frame.kind as u8;
    header[8..].copy_from_slice(&length.to_be_bytes());
    writer
        .write_all(&header)
        .await
        .map_err(|_| WireFailure::Io)?;
    writer
        .write_all(&frame.payload)
        .await
        .map_err(|_| WireFailure::Io)?;
    writer.flush().await.map_err(|_| WireFailure::Io)
}

async fn read_exact<R: AsyncRead + Unpin>(
    reader: &mut R,
    bytes: &mut [u8],
) -> Result<(), WireFailure> {
    reader.read_exact(bytes).await.map(|_| ()).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            WireFailure::Truncated
        } else {
            WireFailure::Io
        }
    })
}

fn validate_length(kind: FrameKind, length: usize) -> Result<(), WireFailure> {
    match kind {
        FrameKind::Control if !(2..=MAX_CONTROL_BYTES).contains(&length) => {
            Err(WireFailure::Length)
        }
        FrameKind::Audio if !(1..=MAX_AUDIO_BYTES).contains(&length) => Err(WireFailure::Length),
        _ => Ok(()),
    }
}

fn valid_request_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn decode_strict_json(payload: &[u8]) -> Result<Value, WireFailure> {
    let mut deserializer = serde_json::Deserializer::from_slice(payload);
    let value = StrictValue::deserialize(&mut deserializer).map_err(|_| WireFailure::Control)?;
    deserializer.end().map_err(|_| WireFailure::Control)?;
    Ok(value.0)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut keys = HashSet::new();
        while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            values.insert(key, value.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_rejected_recursively() {
        assert!(decode_strict_json(br#"{"kind":"request","kind":"event"}"#).is_err());
        assert!(decode_strict_json(br#"{"outer":{"x":1,"x":2}}"#).is_err());
    }

    #[test]
    fn unknown_envelope_members_are_ignored() {
        let request = Request::parse(
            br#"{"kind":"request","id":"r","method":"runtime.health","params":{},"future":true}"#,
        )
        .unwrap();
        assert_eq!(request.id, "r");
    }
}
