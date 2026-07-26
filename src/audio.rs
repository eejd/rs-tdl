use anyhow::{anyhow, Context, Error};
use std::path::Path;

struct SymphoniaI32Source {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    channels: usize,
    bits_per_sample: usize,
    sample_rate: usize,
    pending: Vec<i32>,
    eof: bool,
}

impl SymphoniaI32Source {
    fn decode_one(&mut self) -> Result<bool, Error> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(symphonia::core::errors::Error::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.eof = true;
                    return Ok(false);
                }
                Err(error) => return Err(anyhow!("unable to read source audio packet: {error}")),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = self
                .decoder
                .decode(&packet)
                .map_err(|error| anyhow!("unable to decode source FLAC packet: {error}"))?;
            let spec = *decoded.spec();
            let mut samples =
                symphonia::core::audio::SampleBuffer::<i32>::new(decoded.capacity() as u64, spec);
            samples.copy_interleaved_ref(decoded);
            // Symphonia represents narrower integer samples left-aligned in an i32
            // (for example, i16 -> i32 shifts by 16). flacenc instead expects the
            // original signed integer range described by `bits_per_sample`.
            let shift = 32 - self.bits_per_sample;
            self.pending
                .extend(samples.samples().iter().map(|sample| sample >> shift));
            return Ok(true);
        }
    }
}

impl flacenc::source::Source for SymphoniaI32Source {
    fn channels(&self) -> usize {
        self.channels
    }

    fn bits_per_sample(&self) -> usize {
        self.bits_per_sample
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate
    }

    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        destination: &mut F,
    ) -> Result<usize, flacenc::error::SourceError> {
        let requested = block_size * self.channels;
        while self.pending.len() < requested && !self.eof {
            if self.decode_one().is_err() {
                return Err(flacenc::error::SourceError::by_reason(
                    flacenc::error::SourceErrorReason::InvalidFormat,
                ));
            }
        }
        let count = requested.min(self.pending.len());
        let count = count - (count % self.channels.max(1));
        if count == 0 {
            return Ok(0);
        }
        destination.fill_interleaved(&self.pending[..count])?;
        self.pending.drain(..count);
        Ok(count / self.channels)
    }
}

pub fn convert_containerized_flac(input: &Path, output: &Path) -> Result<(), Error> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let source = std::fs::File::open(input)
        .with_context(|| format!("unable to open {} for FLAC extraction", input.display()))?;
    let media = MediaSourceStream::new(Box::new(source), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            media,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| anyhow!("unable to read source audio container: {error}"))?;
    let format = probed.format;
    let track = format
        .default_track()
        .context("source contained no audio track")?;
    let parameters = track.codec_params.clone();
    if parameters.codec != symphonia::core::codecs::CODEC_TYPE_FLAC {
        return Err(anyhow!(
            "refusing native FLAC conversion because the decoded source codec is not FLAC"
        ));
    }
    let track_id = track.id;
    let sample_rate = parameters
        .sample_rate
        .context("source omitted sample rate")? as usize;
    let channels = parameters
        .channels
        .map(|channels| channels.count())
        .context("source omitted channel layout")?;
    let bits_per_sample = parameters
        .bits_per_sample
        .context("source omitted FLAC bit depth")? as usize;
    if !(4..=32).contains(&bits_per_sample) {
        return Err(anyhow!(
            "unsupported FLAC bit depth for native conversion: {bits_per_sample}"
        ));
    }
    let decoder = symphonia::default::get_codecs()
        .make(&parameters, &DecoderOptions::default())
        .map_err(|error| anyhow!("unable to create FLAC decoder: {error}"))?;
    let source = SymphoniaI32Source {
        format,
        decoder,
        track_id,
        channels,
        bits_per_sample,
        sample_rate,
        pending: Vec::new(),
        eof: false,
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, error)| anyhow!("invalid FLAC encoder configuration: {error}"))?;
    let block_size = config.block_size;
    let stream = flacenc::encode_with_fixed_block_size(&config, source, block_size)
        .map_err(|error| anyhow!("unable to encode native FLAC: {error}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|error| anyhow!("unable to serialize native FLAC: {error}"))?;
    std::fs::write(output, sink.into_inner())
        .with_context(|| format!("unable to write extracted FLAC to {}", output.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::convert_containerized_flac;

    fn source_pcm_identity(path: &std::path::Path) -> ([u8; 16], u64) {
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(path).unwrap();
        let media = MediaSourceStream::new(Box::new(file), Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                media,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .unwrap();
        let track = probed.format.default_track().unwrap();
        let md5 = match track.codec_params.verification_check {
            Some(symphonia::core::codecs::VerificationCheck::Md5(md5)) => md5,
            check => panic!("source omitted its FLAC PCM MD5: {check:?}"),
        };
        (md5, track.codec_params.n_frames.unwrap())
    }

    #[test]
    #[ignore = "requires TDL_CONTAINER_FLAC_FIXTURE"]
    fn converts_external_containerized_flac_fixture() {
        let input = std::env::var("TDL_CONTAINER_FLAC_FIXTURE").unwrap();
        let output = std::env::var_os("TDL_NATIVE_FLAC_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("tdl-container-flac-conversion.flac"));
        convert_containerized_flac(std::path::Path::new(&input), &output).unwrap();
        let data = std::fs::read(&output).unwrap();
        assert!(data.starts_with(b"fLaC"));
        let tag = metaflac::Tag::read_from_path(&output).unwrap();
        let output_info = tag
            .blocks()
            .find_map(|block| match block {
                metaflac::Block::StreamInfo(info) => Some(info),
                _ => None,
            })
            .unwrap();
        let source_pcm = source_pcm_identity(std::path::Path::new(&input));
        assert_eq!(source_pcm.0.as_slice(), output_info.md5.as_slice());
        if source_pcm.1 != 0 {
            assert_eq!(source_pcm.1, output_info.total_samples);
        }
        println!("converted fixture to {}", output.display());
    }
}
