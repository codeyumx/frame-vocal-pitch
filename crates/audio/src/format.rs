//! SPA `EnumFormat` pod for the crate's own streams: F32 interleaved at 48 kHz with the given
//! channel positions (empty = let the graph choose the layout).

use pipewire as pw;
use pw::spa;

use crate::capture::SAMPLE_RATE_HZ;

/// Serializes an `EnumFormat` for `positions` (`SPA_AUDIO_CHANNEL_*` values).
pub(crate) fn format_pod(positions: &[u32]) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE_HZ);
    if !positions.is_empty() {
        info.set_channels(positions.len() as u32);
        let mut pos = [0u32; spa::param::audio::MAX_CHANNELS];
        pos[..positions.len()].copy_from_slice(positions);
        info.set_position(pos);
    }
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .expect("serializing a fixed audio format cannot fail")
    .0
    .into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the serialized pod back into an `AudioInfoRaw`.
    fn roundtrip(positions: &[u32]) -> spa::param::audio::AudioInfoRaw {
        let pod = format_pod(positions);
        let pod = spa::pod::Pod::from_bytes(&pod).expect("pod");
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.parse(pod).expect("audio info");
        info
    }

    #[test]
    fn fixed_format_for_known_layout() {
        let info = roundtrip(&[crate::graph::CHANNEL_MONO]);
        assert_eq!(info.format(), spa::param::audio::AudioFormat::F32LE);
        assert_eq!(info.rate(), SAMPLE_RATE_HZ);
        assert_eq!(info.channels(), 1);
        assert_eq!(info.position()[..1], [crate::graph::CHANNEL_MONO]);
    }

    #[test]
    fn empty_positions_leave_layout_open() {
        let info = roundtrip(&[]);
        assert_eq!(info.format(), spa::param::audio::AudioFormat::F32LE);
        assert_eq!(info.rate(), SAMPLE_RATE_HZ);
        assert_eq!(info.channels(), 0);
    }
}
