use super::models::{EncryptionType, PlaybackManifest};
use anyhow::{anyhow, Context, Error};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Default)]
struct SegmentTemplate {
    initialization: Option<String>,
    media: Option<String>,
    start_number: u64,
    timeline: Vec<(u64, u64)>,
}

pub fn parse(input: &str) -> Result<PlaybackManifest, Error> {
    let mut reader = Reader::from_str(input);
    let mut adaptation_template = SegmentTemplate {
        start_number: 1,
        ..Default::default()
    };
    let mut representation_template = SegmentTemplate {
        start_number: 1,
        ..Default::default()
    };
    let mut in_adaptation = false;
    let mut in_representation = false;
    let mut in_timeline = false;
    let mut timeline_belongs_to_representation = false;
    let mut representation_id = String::new();
    let mut codecs = None;
    let mut mime_type = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"AdaptationSet" => in_adaptation = true,
                b"Representation" if in_adaptation => {
                    in_representation = true;
                    representation_template = SegmentTemplate {
                        initialization: adaptation_template.initialization.clone(),
                        media: adaptation_template.media.clone(),
                        start_number: adaptation_template.start_number,
                        timeline: adaptation_template.timeline.clone(),
                    };
                    read_representation(
                        &event,
                        &mut representation_id,
                        &mut codecs,
                        &mut mime_type,
                    );
                }
                b"SegmentTemplate" => {
                    let target = if in_representation {
                        &mut representation_template
                    } else {
                        &mut adaptation_template
                    };
                    read_template(&event, target)?;
                }
                b"SegmentTimeline" if in_adaptation => {
                    in_timeline = true;
                    timeline_belongs_to_representation = in_representation;
                    if in_representation {
                        representation_template.timeline.clear();
                    }
                }
                b"S" if in_timeline => {
                    let segment = read_timeline_segment(&event)?;
                    if timeline_belongs_to_representation {
                        representation_template.timeline.push(segment);
                    } else {
                        adaptation_template.timeline.push(segment);
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(event)) => match event.local_name().as_ref() {
                b"Representation" if in_adaptation => {
                    read_representation(
                        &event,
                        &mut representation_id,
                        &mut codecs,
                        &mut mime_type,
                    );
                    representation_template = SegmentTemplate {
                        initialization: adaptation_template.initialization.clone(),
                        media: adaptation_template.media.clone(),
                        start_number: adaptation_template.start_number,
                        timeline: adaptation_template.timeline.clone(),
                    };
                }
                b"SegmentTemplate" => {
                    let target = if in_representation {
                        &mut representation_template
                    } else {
                        &mut adaptation_template
                    };
                    read_template(&event, target)?;
                }
                b"S" if in_timeline => {
                    let segment = read_timeline_segment(&event)?;
                    if timeline_belongs_to_representation {
                        representation_template.timeline.push(segment);
                    } else {
                        adaptation_template.timeline.push(segment);
                    }
                }
                _ => {}
            },
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"Representation" => in_representation = false,
                b"SegmentTimeline" => in_timeline = false,
                b"AdaptationSet" => in_adaptation = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(anyhow!("unable to parse DASH XML: {error}")),
            _ => {}
        }
    }

    let template = if representation_template.media.is_some() {
        representation_template
    } else {
        adaptation_template
    };
    let initialization = template
        .initialization
        .context("DASH manifest did not contain an initialization URL")?;
    let media = template
        .media
        .context("DASH manifest did not contain a media URL template")?;
    if template.timeline.is_empty() {
        return Err(anyhow!("DASH manifest did not contain a segment timeline"));
    }

    let expand_static = |value: &str| value.replace("$RepresentationID$", &representation_id);
    let mut urls = vec![expand_static(&initialization)];
    let mut number = template.start_number.max(1);
    for (_, repeats) in template.timeline {
        for _ in 0..=repeats {
            urls.push(expand_static(&media).replace("$Number$", &number.to_string()));
            number += 1;
        }
    }

    Ok(PlaybackManifest {
        mime_type: mime_type.unwrap_or_else(|| "audio/mp4".to_string()),
        codecs: codecs.context("DASH manifest did not report an audio codec")?,
        encryption_type: EncryptionType::None,
        key_id: None,
        urls,
    })
}

fn read_template(event: &BytesStart<'_>, template: &mut SegmentTemplate) -> Result<(), Error> {
    for attribute in event.attributes() {
        let attribute = attribute?;
        let value = attribute.unescape_value()?.into_owned();
        match attribute.key.local_name().as_ref() {
            b"initialization" => template.initialization = Some(value),
            b"media" => template.media = Some(value),
            b"startNumber" => template.start_number = value.parse()?,
            _ => {}
        }
    }
    Ok(())
}

fn read_timeline_segment(event: &BytesStart<'_>) -> Result<(u64, u64), Error> {
    let mut duration = None;
    let mut repeat = 0;
    for attribute in event.attributes() {
        let attribute = attribute?;
        let value = attribute.unescape_value()?.into_owned();
        match attribute.key.local_name().as_ref() {
            b"d" => duration = Some(value.parse()?),
            b"r" => repeat = value.parse()?,
            _ => {}
        }
    }
    Ok((
        duration.context("DASH timeline segment omitted duration")?,
        repeat,
    ))
}

fn read_representation(
    event: &BytesStart<'_>,
    id: &mut String,
    codecs: &mut Option<String>,
    mime_type: &mut Option<String>,
) {
    for attribute in event.attributes().flatten() {
        let value = String::from_utf8_lossy(&attribute.value).into_owned();
        match attribute.key.local_name().as_ref() {
            b"id" => *id = value,
            b"codecs" => *codecs = Some(value),
            b"mimeType" => *mime_type = Some(value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_adaptation_level_tidal_segment_template() {
        let xml = r#"<MPD><Period><AdaptationSet>
            <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg$Number$.m4s" startNumber="1">
              <SegmentTimeline><S d="500" r="1"/></SegmentTimeline>
            </SegmentTemplate>
            <Representation id="audio_flac" codecs="flac" mimeType="audio/mp4"/>
          </AdaptationSet></Period></MPD>"#;
        let manifest = parse(xml).unwrap();
        assert_eq!(manifest.codecs, "flac");
        assert_eq!(manifest.mime_type, "audio/mp4");
        assert_eq!(
            manifest.urls,
            vec![
                "audio_flac/init.mp4",
                "audio_flac/seg1.m4s",
                "audio_flac/seg2.m4s"
            ]
        );
    }
}
