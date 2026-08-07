use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavMetadata {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub data_bytes: u32,
}

#[derive(Debug, Error)]
#[error("eSpeak NG output is not a valid PCM16 RIFF/WAVE file")]
pub struct WavError;

/// Validate PCM16 WAV data and replace eSpeak's streaming size sentinels.
///
/// # Errors
///
/// Returns [`WavError`] when the RIFF/WAVE structure, encoding, lengths, or
/// sample metadata are invalid.
pub fn normalize_pcm16_wav(bytes: &mut [u8]) -> Result<WavMetadata, WavError> {
    let (metadata, streaming_data_size_offset) = inspect_pcm16_wav_inner(bytes)?;
    if let Some(data_size_offset) = streaming_data_size_offset {
        let riff_size =
            u32::try_from(bytes.len().checked_sub(8).ok_or(WavError)?).map_err(|_| WavError)?;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes[data_size_offset..data_size_offset + 4]
            .copy_from_slice(&metadata.data_bytes.to_le_bytes());
    }
    Ok(metadata)
}

/// Validate a finite PCM16 WAV file and return its playback metadata.
///
/// # Errors
///
/// Returns [`WavError`] for malformed, empty, unsupported, or inconsistent WAV.
pub fn inspect_pcm16_wav(bytes: &[u8]) -> Result<WavMetadata, WavError> {
    inspect_pcm16_wav_inner(bytes).map(|(metadata, _)| metadata)
}

/// Encode nonempty mono PCM16 samples as a bounded RIFF/WAVE file.
///
/// # Errors
///
/// Returns [`WavError`] for an invalid sample rate, empty samples, integer
/// overflow, or output exceeding `maximum_bytes`.
pub fn pcm16_mono_wav(
    samples: &[i16],
    sample_rate_hz: u32,
    maximum_bytes: usize,
) -> Result<Vec<u8>, WavError> {
    if samples.is_empty() || !(8_000..=96_000).contains(&sample_rate_hz) {
        return Err(WavError);
    }
    let data_bytes = samples.len().checked_mul(2).ok_or(WavError)?;
    let total_bytes = 44_usize.checked_add(data_bytes).ok_or(WavError)?;
    if total_bytes > maximum_bytes {
        return Err(WavError);
    }
    let data_bytes = u32::try_from(data_bytes).map_err(|_| WavError)?;
    let mut bytes = Vec::with_capacity(total_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36_u32.checked_add(data_bytes).ok_or(WavError)?).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&sample_rate_hz.checked_mul(2).ok_or(WavError)?.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

fn inspect_pcm16_wav_inner(bytes: &[u8]) -> Result<(WavMetadata, Option<usize>), WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError);
    }
    let riff_size = read_u32(bytes, 4)?;
    let streaming_lengths = matches!(riff_size, u32::MAX | 0x7fff_f000);
    if !streaming_lengths && (riff_size as usize).checked_add(8) != Some(bytes.len()) {
        return Err(WavError);
    }

    let mut position = 12_usize;
    let mut format = None;
    let mut data_bytes = None;
    let mut streaming_data_length = false;
    let mut streaming_data_size_offset = None;
    while position < bytes.len() {
        let header_end = position.checked_add(8).ok_or(WavError)?;
        if header_end > bytes.len() {
            return Err(WavError);
        }
        let id = &bytes[position..position + 4];
        let declared_size = read_u32(bytes, position + 4)?;
        let payload_start = header_end;
        let streaming_data = id == b"data" && matches!(declared_size, u32::MAX | 0x7fff_f000);
        if streaming_data && !streaming_lengths {
            return Err(WavError);
        }
        let size = if streaming_data {
            bytes.len().checked_sub(payload_start).ok_or(WavError)?
        } else {
            declared_size as usize
        };
        let payload_end = payload_start.checked_add(size).ok_or(WavError)?;
        if payload_end > bytes.len() {
            return Err(WavError);
        }

        if id == b"fmt " {
            if format.is_some() || size < 16 {
                return Err(WavError);
            }
            let encoding = read_u16(bytes, payload_start)?;
            let channels = read_u16(bytes, payload_start + 2)?;
            let sample_rate_hz = read_u32(bytes, payload_start + 4)?;
            let byte_rate = read_u32(bytes, payload_start + 8)?;
            let block_align = read_u16(bytes, payload_start + 12)?;
            let bits = read_u16(bytes, payload_start + 14)?;
            let expected_align = channels.checked_mul(2).ok_or(WavError)?;
            let expected_rate = sample_rate_hz
                .checked_mul(u32::from(expected_align))
                .ok_or(WavError)?;
            if encoding != 1
                || !(1..=2).contains(&channels)
                || !(8_000..=96_000).contains(&sample_rate_hz)
                || bits != 16
                || block_align != expected_align
                || byte_rate != expected_rate
            {
                return Err(WavError);
            }
            format = Some((sample_rate_hz, channels, expected_align));
        } else if id == b"data" {
            if data_bytes
                .replace(u32::try_from(size).map_err(|_| WavError)?)
                .is_some()
            {
                return Err(WavError);
            }
            streaming_data_length = streaming_data;
            streaming_data_size_offset = streaming_data.then_some(position + 4);
        }

        position = payload_end.checked_add(size & 1).ok_or(WavError)?;
        if position > bytes.len() {
            return Err(WavError);
        }
    }
    if position != bytes.len() || streaming_lengths != streaming_data_length {
        return Err(WavError);
    }
    let (sample_rate_hz, channels, block_align) = format.ok_or(WavError)?;
    let data_bytes = data_bytes.ok_or(WavError)?;
    if data_bytes == 0 || data_bytes % u32::from(block_align) != 0 {
        return Err(WavError);
    }
    Ok((
        WavMetadata {
            sample_rate_hz,
            channels,
            data_bytes,
        },
        streaming_data_size_offset,
    ))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, WavError> {
    let value = bytes.get(offset..offset + 2).ok_or(WavError)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, WavError> {
    let value = bytes.get(offset..offset + 4).ok_or(WavError)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16]) -> Vec<u8> {
        let data_size = u32::try_from(samples.len() * 2).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&22_050_u32.to_le_bytes());
        bytes.extend_from_slice(&44_100_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn normalizes_espeak_streaming_lengths() {
        let expected = wav(&[0, 1, -1]);
        for sentinel in [u32::MAX, 0x7fff_f000] {
            let mut actual = expected.clone();
            actual[4..8].copy_from_slice(&sentinel.to_le_bytes());
            actual[40..44].copy_from_slice(&sentinel.to_le_bytes());
            let metadata = normalize_pcm16_wav(&mut actual).unwrap();
            assert_eq!(metadata.sample_rate_hz, 22_050);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn rejects_invalid_or_empty_audio() {
        assert!(normalize_pcm16_wav(&mut []).is_err());
        assert!(normalize_pcm16_wav(&mut wav(&[])).is_err());
        let mut compressed = wav(&[0]);
        compressed[20..22].copy_from_slice(&3_u16.to_le_bytes());
        assert!(normalize_pcm16_wav(&mut compressed).is_err());
    }

    #[test]
    fn writes_bounded_pcm16_wav() {
        let bytes = pcm16_mono_wav(&[0, 1, -1], 22_050, 50).unwrap();
        assert_eq!(inspect_pcm16_wav(&bytes).unwrap().data_bytes, 6);
        assert!(pcm16_mono_wav(&[0, 1, -1], 22_050, 49).is_err());
    }
}
