use super::{
    base64_url_decode, open_object_revision_payload, CloudRuntimeError, DeviceAudioCodec,
    ObjectContentKind, ObjectRevisionCryptoContext, ObjectRevisionEnvelope, StreamingSha256,
    MAX_ENCRYPTED_CHUNK_BYTES, MAX_PLAINTEXT_CHUNK_BYTES,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use zeroize::Zeroizing;

pub(super) const SOURCE_FORMAT: &str = "sona-thought-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThoughtManifest {
    pub format_version: u32,
    pub kind: String,
    pub device_id: String,
    pub captured_at_utc_ms: i64,
    pub origin: ThoughtOrigin,
    pub text: String,
    pub link: Option<ThoughtLink>,
    pub audio: Option<ThoughtAudio>,
    pub images: Vec<ThoughtImage>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ThoughtOrigin {
    Voice,
    Typed,
    Shared,
}

impl ThoughtOrigin {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Typed => "typed",
            Self::Shared => "shared",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThoughtLink {
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThoughtAudio {
    pub codec: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub byte_length: u64,
    pub sha256: String,
    pub duration_ms: i64,
    pub chunk_start: u32,
    pub chunk_count: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThoughtImage {
    pub mime: String,
    pub byte_length: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub chunk_start: u32,
    pub chunk_count: u32,
}

pub(super) struct Attachment<'a> {
    pub start: u32,
    pub count: u32,
    pub length: u64,
    pub sha256: &'a str,
}

impl ThoughtManifest {
    pub(super) fn attachments(&self) -> Vec<Attachment<'_>> {
        let mut attachments =
            Vec::with_capacity(self.images.len() + usize::from(self.audio.is_some()));
        if let Some(audio) = &self.audio {
            attachments.push(Attachment {
                start: audio.chunk_start,
                count: audio.chunk_count,
                length: audio.byte_length,
                sha256: &audio.sha256,
            });
        }
        attachments.extend(self.images.iter().map(|image| Attachment {
            start: image.chunk_start,
            count: image.chunk_count,
            length: image.byte_length,
            sha256: &image.sha256,
        }));
        attachments.sort_unstable_by_key(|attachment| attachment.start);
        attachments
    }

    pub(super) fn validate(
        &self,
        envelope: &ObjectRevisionEnvelope,
    ) -> Result<(), CloudRuntimeError> {
        if self.format_version != 1
            || self.kind != "thought"
            || self.device_id != envelope.writer_device_id
            || self.captured_at_utc_ms < 0
            || envelope.chunk_count == 0
            || envelope.chunk_count > 4096
            || envelope.total_bytes > 8 * 1024 * 1024 * 1024
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        if let Some(audio) = &self.audio {
            let codec = DeviceAudioCodec::parse(&audio.codec, audio.sample_rate_hz, audio.channels)
                .ok_or(CloudRuntimeError::IntegrityFailure)?;
            if !codec.admits_length(audio.byte_length) || audio.duration_ms < 0 {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
        }
        if self
            .images
            .iter()
            .any(|image| image.width == 0 || image.height == 0 || !image.mime.starts_with("image/"))
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let mut next = 0_u32;
        for attachment in self.attachments() {
            let expected_count = attachment
                .length
                .div_ceil(MAX_PLAINTEXT_CHUNK_BYTES as u64)
                .max(1);
            if attachment.start != next
                || u64::from(attachment.count) != expected_count
                || base64_url_decode(attachment.sha256).map_or(true, |hash| hash.len() != 32)
            {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            next = next
                .checked_add(attachment.count)
                .ok_or(CloudRuntimeError::IntegrityFailure)?;
        }
        if next.max(1) != envelope.chunk_count {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(())
    }
}

pub(super) fn open_manifest(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
) -> Result<Option<ThoughtManifest>, CloudRuntimeError> {
    let context = ObjectRevisionCryptoContext {
        source_format: SOURCE_FORMAT,
        ..*context
    };
    let Ok(bytes) = open_object_revision_payload(vault_root, &context, ciphertext) else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| CloudRuntimeError::IntegrityFailure)
}

pub(super) fn open_chunk(
    vault_root: &[u8],
    context: &ObjectRevisionCryptoContext<'_>,
    ciphertext: &[u8],
    attachment: Option<&Attachment<'_>>,
) -> Result<Zeroizing<Vec<u8>>, CloudRuntimeError> {
    if ciphertext.len() > MAX_ENCRYPTED_CHUNK_BYTES {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    let context = ObjectRevisionCryptoContext {
        source_format: SOURCE_FORMAT,
        content_kind: ObjectContentKind::Chunk,
        ..*context
    };
    let mut bytes = Zeroizing::new(
        open_object_revision_payload(vault_root, &context, ciphertext)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    );
    let Some(attachment) = attachment else {
        return if bytes.is_empty() {
            Ok(bytes)
        } else {
            Err(CloudRuntimeError::IntegrityFailure)
        };
    };
    let offset = context
        .index
        .checked_sub(u64::from(attachment.start))
        .and_then(|index| index.checked_mul(MAX_PLAINTEXT_CHUNK_BYTES as u64))
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
    let length = attachment
        .length
        .checked_sub(offset)
        .ok_or(CloudRuntimeError::IntegrityFailure)?
        .min(MAX_PLAINTEXT_CHUNK_BYTES as u64) as usize;
    if bytes.len() < length
        || bytes[length..].iter().any(|byte| *byte != 0)
        || (bytes.len() != length && bytes.len() != MAX_PLAINTEXT_CHUNK_BYTES)
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    bytes.truncate(length);
    Ok(bytes)
}

pub(super) struct AttachmentDigests<'a> {
    attachments: Vec<Attachment<'a>>,
    current: usize,
    digest: StreamingSha256,
}

impl<'a> AttachmentDigests<'a> {
    pub(super) fn new(manifest: &'a ThoughtManifest) -> Self {
        Self {
            attachments: manifest.attachments(),
            current: 0,
            digest: StreamingSha256::default(),
        }
    }

    pub(super) fn receive(
        &mut self,
        vault_root: &[u8],
        context: &ObjectRevisionCryptoContext<'_>,
        ciphertext: &[u8],
    ) -> Result<(), CloudRuntimeError> {
        let attachment = self.attachments.get(self.current);
        let bytes = open_chunk(vault_root, context, ciphertext, attachment)?;
        self.digest.update(&bytes);
        if attachment.is_some_and(|attachment| {
            context.index + 1 == u64::from(attachment.start) + u64::from(attachment.count)
        }) {
            let actual = std::mem::take(&mut self.digest).finish_base64url();
            if Some(actual.as_str()) != attachment.map(|attachment| attachment.sha256) {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
            self.current += 1;
        }
        Ok(())
    }
}

/// Decode a phone's voice source to the mono 16 kHz samples the selected ASR
/// receives. Both an older phone's raw PCM and the compact stream stay local.
pub(crate) fn thought_audio_samples(
    codec: &str,
    sample_rate_hz: u32,
    channels: u16,
    bytes: &[u8],
) -> Option<Vec<f32>> {
    use crate::managers::media_import::{decode_media_stream_into, MAX_MEDIA_IMPORT_SAMPLES};
    use std::{io::Cursor, sync::atomic::AtomicBool};
    use symphonia::core::io::MediaSourceStream;

    let codec = DeviceAudioCodec::parse(codec, sample_rate_hz, channels)?;
    if bytes.is_empty() || !codec.admits_length(u64::try_from(bytes.len()).ok()?) {
        return None;
    }
    match codec {
        DeviceAudioCodec::Pcm => {
            if bytes.len() / 2 > MAX_MEDIA_IMPORT_SAMPLES {
                return None;
            }
            Some(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
                    .collect(),
            )
        }
        DeviceAudioCodec::AacAdts => {
            let mut samples = Vec::new();
            decode_media_stream_into(
                MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default()),
                codec.extension(),
                &AtomicBool::new(false),
                MAX_MEDIA_IMPORT_SAMPLES,
                |frame| {
                    samples.extend_from_slice(frame);
                    Ok(())
                },
            )
            .ok()?;
            Some(samples)
        }
    }
}

#[cfg(test)]
mod audio_tests {
    use super::thought_audio_samples;

    #[test]
    fn an_older_phones_pcm_decodes_to_the_same_samples() {
        let pcm: Vec<u8> = [i16::MIN, -16_384, 0, 16_384, i16::MAX]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        assert_eq!(
            thought_audio_samples("pcm_s16le", 16_000, 1, &pcm).unwrap(),
            vec![-1.0, -0.5, 0.0, 0.5, 32767.0 / 32768.0]
        );
    }

    #[test]
    fn a_compact_phones_voice_decodes_to_speech_samples() {
        let bytes = include_bytes!("../../tests/fixtures/phone-audio/jfk.aac");
        let samples = thought_audio_samples("aac_adts", 16_000, 1, bytes).unwrap();
        assert!(
            (176_000..180_000).contains(&samples.len()),
            "{} samples",
            samples.len()
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
        let reference = hound::WavReader::new(std::io::Cursor::new(include_bytes!(
            "../../tests/fixtures/phone-audio/jfk.wav"
        )))
        .unwrap()
        .samples::<i16>()
        .map(|sample| f64::from(sample.unwrap()) / 32768.0)
        .collect::<Vec<_>>();
        // Apple's AAC-LC encoder primes with 2,112 samples (132 ms at 16 kHz).
        let decoded = &samples[2112..2112 + reference.len()];
        let dot = reference
            .iter()
            .zip(decoded)
            .map(|(left, right)| left * f64::from(*right))
            .sum::<f64>();
        let left_energy = reference.iter().map(|sample| sample * sample).sum::<f64>();
        let right_energy = decoded
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>();
        let correlation = dot / (left_energy * right_energy).sqrt();
        assert!(
            correlation > 0.95,
            "speech waveform correlation: {correlation}"
        );
    }

    #[test]
    fn unsupported_or_broken_voice_is_refused() {
        assert!(thought_audio_samples("aac_adts", 16_000, 1, b"not audio").is_none());
        assert!(thought_audio_samples("aac_adts", 48_000, 1, &[0; 20]).is_none());
        assert!(thought_audio_samples("pcm_s16le", 16_000, 1, &[0; 3]).is_none());
        assert!(thought_audio_samples("pcm_s16le", 16_000, 2, &[0; 4]).is_none());
        assert!(thought_audio_samples("opus", 16_000, 1, &[0; 20]).is_none());
    }
}
