use super::{models::*, ApiClient};
use anyhow::anyhow;
use anyhow::Error;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use tokio::try_join;

fn validate_requested_quality(
    requested: AudioQuality,
    returned: AudioQuality,
    manifest: &PlaybackManifest,
    profile_name: &str,
) -> Result<(), Error> {
    let requested_lossless = matches!(requested, AudioQuality::Lossless | AudioQuality::HiRes);
    let returned_lossless = matches!(returned, AudioQuality::Lossless | AudioQuality::HiRes)
        && manifest.is_lossless()?;

    if requested_lossless && !returned_lossless {
        return Err(anyhow!(
            "TIDAL quality downgrade on OAuth profile '{profile_name}': requested {requested}, returned {returned} ({}/{}); AAC-LC is lossy even in an MP4 container. Relink with `tdl login --profile {profile_name} --force` if the official app offers lossless",
            manifest.mime_type,
            manifest.codecs,
        ));
    }
    Ok(())
}

pub struct MediaClient(Arc<ApiClient>);

impl MediaClient {
    pub fn new(client: Arc<ApiClient>) -> Self {
        Self(client)
    }
}

impl Deref for MediaClient {
    type Target = ApiClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl MediaClient {
    pub async fn get_track(&self, id: &str) -> Result<Track, Error> {
        let url = format!("{}/tracks/{}", self.api_base, id);
        self.get::<Track>(&url, None).await
    }

    pub async fn get_album(&self, id: usize) -> Result<Album, Error> {
        let url = format!("{}/albums/{}", self.api_base, id);
        self.get::<Album>(&url, None).await
    }

    pub async fn get_playlist(&self, id: &str) -> Result<Playlist, Error> {
        let url = format!("https://api.tidal.com/v1/playlists/{id}");
        self.get::<Playlist>(&url, None).await
    }

    pub async fn get_user_playlists(&self) -> Result<Vec<PlaylistSummary>, Error> {
        let session = self
            .get::<UserResponse>("https://api.tidal.com/v1/sessions", None)
            .await?;
        let url = format!(
            "https://api.tidal.com/v1/users/{}/playlists",
            session.user_id
        );
        self.get_items::<PlaylistSummary>(&url, None, None).await
    }

    pub async fn get_artist(&self, id: &str) -> Result<Artist, Error> {
        let url = format!("https://api.tidal.com/v1/artists/{id}");
        self.get::<Artist>(&url, None).await
    }
    pub async fn get_stream_url(&self, id: usize) -> Result<PlaybackManifest, Error> {
        let url = format!("{}/tracks/{}/playbackinfopostpaywall", self.api_base, id);
        let query = &[
            ("audioquality".to_string(), self.audio_quality.to_string()),
            ("playbackmode".to_string(), PlaybackMode::Stream.to_string()),
            (
                "assetpresentation".to_string(),
                AssetPresentation::Full.to_string(),
            ),
        ];

        // Playback manifests contain short-lived signed URLs and must never be
        // served from the persistent metadata cache.
        let req = self
            .get_uncached::<PlaybackInfoPostPaywallRes>(&url, Some(query))
            .await?;

        match req.manifest_mime_type.as_str() {
            "application/vnd.tidal.bts" => {
                let manifest = PlaybackManifest::from_str(&req.manifest)?;
                let requested = self.audio_quality;
                let returned = req.audio_quality;
                validate_requested_quality(requested, returned, &manifest, &self.profile_name)?;

                Ok(manifest)
            }
            "application/dash+xml" => {
                let decoded = base64::decode(&req.manifest)?;
                let xml = String::from_utf8(decoded)?;
                let manifest = super::dash::parse(&xml)?;
                validate_requested_quality(
                    self.audio_quality,
                    req.audio_quality,
                    &manifest,
                    &self.profile_name,
                )?;
                Ok(manifest)
            }
            mime_type => Err(anyhow!(
                "unsupported TIDAL playback manifest type: {mime_type}"
            )),
        }
    }

    pub async fn get_artist_albums(&self, id: &str) -> Result<Vec<Album>, Error> {
        let url = format!("https://api.tidal.com/v1/artists/{id}/albums");
        let mut albums: Vec<Album> = Vec::new();
        let album_req = self.get_items::<Album>(&url, None, None);
        if self.include_singles {
            let filter = vec![("filter".to_string(), "EPSANDSINGLES".to_string())];
            let singles = self.get_items::<Album>(&url, Some(filter), None);
            //execute the two requests concurrently
            let results = try_join!(album_req, singles)?;

            //add the elements to the results vec
            for mut result in [results.0, results.1] {
                albums.append(&mut result);
            }
        } else {
            //else execute the single request
            albums = album_req.await?;
        }

        Ok(albums)
    }

    fn get_cover_url(id: &str, width: usize, height: usize) -> String {
        format!(
            "https://resources.tidal.com/images/{}/{}x{}.jpg",
            id.replace('-', "/"),
            width,
            height
        )
    }

    pub async fn get_cover_data(&self, id: &str) -> Result<Cover, Error> {
        let req = self
            .http_client
            .get(MediaClient::get_cover_url(id, 1280, 1280))
            .send()
            .await?;

        let content_type = match req.headers().get("Content-Type") {
            Some(val) => val.to_str()?.to_string(),
            None => return Err(anyhow!("Unable to get Content Type from Request")),
        };

        let data = req.bytes().await?.to_vec();
        Ok(Cover { content_type, data })
    }
}

#[cfg(test)]
mod tests {
    use super::validate_requested_quality;
    use crate::api::models::{AudioFormat, AudioQuality, EncryptionType, PlaybackManifest};

    fn manifest(mime_type: &str, codecs: &str) -> PlaybackManifest {
        PlaybackManifest {
            mime_type: mime_type.into(),
            codecs: codecs.into(),
            encryption_type: EncryptionType::None,
            key_id: None,
            urls: vec!["https://media.invalid/track".into()],
        }
    }

    #[test]
    fn native_flac_is_accepted_as_lossless() {
        let value = manifest("audio/flac", "flac");
        assert_eq!(value.audio_format().unwrap(), AudioFormat::Flac);
        validate_requested_quality(
            AudioQuality::Lossless,
            AudioQuality::Lossless,
            &value,
            "test",
        )
        .unwrap();
        assert_eq!(value.get_file_extension(), Some("flac"));
    }

    #[test]
    fn alac_in_mp4_is_accepted_as_lossless() {
        let value = manifest("audio/mp4", "alac");
        assert_eq!(value.audio_format().unwrap(), AudioFormat::Mp4Alac);
        validate_requested_quality(
            AudioQuality::Lossless,
            AudioQuality::Lossless,
            &value,
            "test",
        )
        .unwrap();
        assert_eq!(value.get_file_extension(), Some("m4a"));
    }

    #[test]
    fn flac_in_mp4_is_classified_for_native_extraction() {
        let value = manifest("audio/mp4", "flac");
        assert_eq!(value.audio_format().unwrap(), AudioFormat::ContainerFlac);
        validate_requested_quality(
            AudioQuality::Lossless,
            AudioQuality::Lossless,
            &value,
            "test",
        )
        .unwrap();
        assert_eq!(value.get_file_extension(), Some("m4a"));
    }

    #[test]
    fn aac_lc_is_rejected_for_a_lossless_request() {
        let value = manifest("audio/mp4", "mp4a.40.2");
        assert_eq!(value.audio_format().unwrap(), AudioFormat::Mp4Aac);
        let error =
            validate_requested_quality(AudioQuality::Lossless, AudioQuality::High, &value, "test")
                .unwrap_err()
                .to_string();
        assert!(error.contains("AAC-LC is lossy"));
        assert!(error.contains("--profile test --force"));
    }

    #[test]
    fn contradictory_lossless_aac_manifest_is_rejected() {
        let value = manifest("audio/mp4", "mp4a.40.2");
        assert!(validate_requested_quality(
            AudioQuality::Lossless,
            AudioQuality::Lossless,
            &value,
            "test",
        )
        .is_err());
    }

    #[test]
    fn aac_is_accepted_when_high_quality_was_requested() {
        let value = manifest("audio/mp4", "mp4a.40.2");
        validate_requested_quality(AudioQuality::High, AudioQuality::High, &value, "test").unwrap();
        assert_eq!(value.get_file_extension(), Some("m4a"));
    }
}
