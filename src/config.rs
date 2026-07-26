use crate::api::models::Album;
use crate::api::models::Artist;
use crate::api::models::AudioQuality;
use crate::api::models::Track;
use anyhow::Error;
use config::{Config, File, FileFormat};
use phf::phf_map;
use sanitize_filename::sanitize;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use serde_with::NoneAsEmptyString;
use std::collections::BTreeMap;
use std::env::var;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Settings {
    pub audio_quality: AudioQuality,
    pub flac_output: FlacOutput,
    pub show_progress: bool,
    pub progress_refresh_rate: u8,
    pub include_singles: bool,
    pub downloads: u8,
    pub workers: u8,
    pub download_cover: bool,
    pub cache_dir: String,
    // TOML 0.5 requires root scalar values to be emitted before tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_profile: Option<String>,
    pub download_paths: DownloadPathSettings,
    pub login_key: LoginKey,
    pub api_key: ApiKey,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub auth_profiles: BTreeMap<String, AuthProfile>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum FlacOutput {
    Native,
    Source,
}

impl Settings {
    pub fn save(&self) -> Result<(), Error> {
        let config_file = get_config_file();
        let config_dir = get_config_dir();
        let cache_dir = get_cache_dir();
        // Serialize before opening the destination so an encoding error can
        // never truncate the user's working configuration.
        let config_str = toml::to_string_pretty(&self)?;

        std::fs::create_dir_all(config_dir)?;
        std::fs::create_dir_all(cache_dir)?;

        let temporary_file = format!("{config_file}.tmp");
        let mut file = std::fs::File::create(&temporary_file)?;
        file.write_all(config_str.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temporary_file, config_file)?;
        Ok(())
    }
}
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoginKey {
    #[serde_as(as = "NoneAsEmptyString")]
    pub device_code: Option<String>,
    pub user_id: Option<i64>,
    #[serde_as(as = "NoneAsEmptyString")]
    pub session_id: Option<String>,
    #[serde_as(as = "NoneAsEmptyString")]
    pub country_code: Option<String>,
    #[serde_as(as = "NoneAsEmptyString")]
    pub access_token: Option<String>,
    #[serde_as(as = "NoneAsEmptyString")]
    pub refresh_token: Option<String>,
    pub expires_after: Option<i64>,
    #[serde_as(as = "NoneAsEmptyString")]
    pub auth_profile: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ApiKey {
    pub client_id: String,
    pub client_secret: String,
}

pub const ANDROID_AUTO_PROFILE: &str = "android-auto";
pub const ANDROID_PKCE_PROFILE: &str = "android-pkce";
const ANDROID_AUTO_CLIENT_ID: &str = "zU4XHVVkc2tDPo4t";
const ANDROID_AUTO_CLIENT_SECRET: &str = "VJKhDFqJPqvsPVNBV6ukXTJmwlvbttP7wlMlrc72se4=";
const ANDROID_AUTO_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 12; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36";
const ANDROID_PKCE_CLIENT_ID_PARTS: (&[u8], &[u8]) = (b"TmtKRVUxSmtjRXM=", b"NWFIRkZRbFJuVlE9PQ==");
const ANDROID_PKCE_CLIENT_SECRET_PARTS: (&[u8], &[u8]) = (
    b"ZUdWMVVHMVpOMjVpY0ZvNVNVbGlURUZqVVQ=",
    b"a3pjMmhyWVRGV1RtaGxWVUZ4VGpaSlkzTjZhbFJIT0QwPQ==",
);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthFlow {
    #[default]
    Device,
    Pkce,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthProfile {
    pub client_id: String,
    pub client_secret: String,
    pub user_agent: String,
    pub client_version: String,
    #[serde(default)]
    pub auth_flow: AuthFlow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
}

impl AuthProfile {
    pub fn android_auto() -> Self {
        Self {
            client_id: ANDROID_AUTO_CLIENT_ID.to_string(),
            client_secret: ANDROID_AUTO_CLIENT_SECRET.to_string(),
            user_agent: ANDROID_AUTO_USER_AGENT.to_string(),
            client_version: "2025.7.16".to_string(),
            auth_flow: AuthFlow::Device,
            platform: None,
            device_type: None,
        }
    }

    pub fn android_pkce() -> Result<Self, Error> {
        Ok(Self {
            client_id: decode_joined_base64(ANDROID_PKCE_CLIENT_ID_PARTS)?,
            client_secret: decode_joined_base64(ANDROID_PKCE_CLIENT_SECRET_PARTS)?,
            user_agent: ANDROID_AUTO_USER_AGENT.to_string(),
            client_version: "2025.7.16".to_string(),
            auth_flow: AuthFlow::Pkce,
            platform: None,
            device_type: None,
        })
    }

    fn validate(&self, name: &str) -> Result<(), Error> {
        if self.client_id.trim().is_empty()
            || self.client_secret.trim().is_empty()
            || self.user_agent.trim().is_empty()
            || self.client_version.trim().is_empty()
        {
            anyhow::bail!(
                "OAuth profile '{name}' must define client_id, client_secret, user_agent, and client_version"
            );
        }
        Ok(())
    }
}

fn decode_joined_base64(parts: (&[u8], &[u8])) -> Result<String, Error> {
    let first = base64::decode(parts.0)?;
    let second = base64::decode(parts.1)?;
    let joined = [first, second].concat();
    Ok(String::from_utf8(base64::decode(joined)?)?)
}

impl LoginKey {
    pub fn clear(&mut self) {
        self.device_code = None;
        self.user_id = None;
        self.session_id = None;
        self.country_code = None;
        self.access_token = None;
        self.refresh_token = None;
        self.expires_after = None;
        self.auth_profile = None;
    }
}

impl Settings {
    pub fn resolve_auth_profile(&self) -> Result<(String, AuthProfile), Error> {
        let selected = self.auth_profile.as_deref();
        let resolved = match selected {
            Some(ANDROID_AUTO_PROFILE) => AuthProfile::android_auto(),
            Some(ANDROID_PKCE_PROFILE) => AuthProfile::android_pkce()?,
            Some(name) => self.auth_profiles.get(name).cloned().ok_or_else(|| {
                anyhow::anyhow!("OAuth profile '{name}' is not defined in auth_profiles")
            })?,
            None if self.api_key.client_id == ANDROID_AUTO_CLIENT_ID
                && self.api_key.client_secret == ANDROID_AUTO_CLIENT_SECRET =>
            {
                AuthProfile::android_auto()
            }
            None => anyhow::bail!(
                "legacy custom OAuth credentials have no request profile; define [auth_profiles.<name>] with the matching request identity, set auth_profile = '<name>', then run `tdl login --force`"
            ),
        };
        let name = selected.unwrap_or(ANDROID_AUTO_PROFILE).to_string();
        resolved.validate(&name)?;
        Ok((name, resolved))
    }
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DownloadPathSettings {
    /// Root used for playlist downloads.
    pub base_path: String,
    /// Root used for direct track, album, and artist downloads.
    pub artist_base_path: String,
    pub playlist: String,
    pub playlist_track: String,
    pub artist: String,
    pub album: String,
    pub track: String,
}

impl DownloadPathSettings {
    pub fn get_destination_root(&self, playlist: bool) -> Result<PathBuf, anyhow::Error> {
        let configured = if playlist {
            &self.base_path
        } else {
            &self.artist_base_path
        };
        Ok(Path::new("").join(shellexpand::full(configured)?.to_string()))
    }

    pub fn get_base_path(&self) -> Result<PathBuf, anyhow::Error> {
        self.get_destination_root(true)
    }
    pub fn get_artist_path(&self, artist: Artist) -> Result<PathBuf, anyhow::Error> {
        let base = self.get_destination_root(false)?;
        Ok(base.join(artist.replace_path(&self.artist)))
    }
    pub fn get_album_path(&self, album: Album, artist: Artist) -> Result<PathBuf, anyhow::Error> {
        let base = &self.get_artist_path(artist.clone())?;
        let path = artist.replace_path(&self.album);
        Ok(base.join(album.replace_path(&path)))
    }
    pub fn get_track_path(
        &self,
        track: Track,
        album: Album,
        artist: Artist,
    ) -> Result<PathBuf, anyhow::Error> {
        if track.playlist.is_some() {
            let base = &self.get_base_path()?;
            let playlist = track.clone().replace_path(&self.playlist);
            let path = track.replace_path(&self.playlist_track);
            let path = album.replace_path(&path);
            return Ok(base.join(playlist).join(artist.replace_path(&path)));
        }

        let base = &self.get_album_path(album.clone(), artist.clone())?;
        let mut path = track.replace_path(&self.track);
        path = album.replace_path(&path);
        path = artist.replace_path(&path);
        Ok(base.join(path))
    }
}

trait UnwrapEmptyString<T: ToString> {
    fn unwrap_empty_string(self) -> String;
}

impl<T> UnwrapEmptyString<T> for Option<T>
where
    T: ToString,
{
    fn unwrap_empty_string(self) -> String {
        match self {
            Some(val) => val.to_string(),
            None => String::new(),
        }
    }
}

pub trait DownloadPath<T>
where
    Self: Sized + Clone,
    T: TokenMap<Self> + 'static + Copy,
{
    fn replace_path(self, path: &str) -> String {
        let map = T::token_map();
        let mut x = path.to_string();
        map.entries().for_each(|entry| {
            if x.contains(entry.0) {
                x = x.replace(entry.0, &entry.1.get_token(&self));
            };
        });
        x
    }
}

impl DownloadPath<ArtistTokens> for Artist {}
impl DownloadPath<AlbumTokens> for Album {}
impl DownloadPath<TrackTokens> for Track {}

pub trait TokenMap<T>
where
    Self: Sized,
    T: Clone,
{
    fn token_map() -> &'static phf::Map<&'static str, Self>;

    fn get_token(self, _: &T) -> String;
}

static ARTIST_TOKEN_MAP: phf::Map<&'static str, ArtistTokens> = phf_map! {
    "{artist_name}" =>  ArtistTokens::Name,
    "{artist_id}" => ArtistTokens::ID
};

#[derive(Clone, Copy)]
pub enum ArtistTokens {
    ID,
    Name,
}

impl TokenMap<Artist> for ArtistTokens {
    fn token_map() -> &'static phf::Map<&'static str, Self> {
        &ARTIST_TOKEN_MAP
    }

    fn get_token(self, a: &Artist) -> String {
        let val = match self {
            ArtistTokens::ID => a.id.to_string(),
            ArtistTokens::Name => a.name.to_string(),
        };
        sanitize(val)
    }
}

static ALBUM_TOKEN_MAP: phf::Map<&'static str, AlbumTokens> = phf_map! {
    "{album_id}" => AlbumTokens::ID,
    "{album_name}" => AlbumTokens::Title,
    "{album_duration}" => AlbumTokens::Duration,
    "{album_tracks}" => AlbumTokens::NumberOfTracks,
    "{album_explicit}" => AlbumTokens::Explicit,
    "{album_quality}" => AlbumTokens::AudioQuality,
    "{album_release}" => AlbumTokens::ReleaseDate,
    "{album_release_year}" => AlbumTokens::ReleaseYear,
};
impl TokenMap<Album> for AlbumTokens {
    fn token_map() -> &'static phf::Map<&'static str, Self> {
        &ALBUM_TOKEN_MAP
    }

    fn get_token(self, a: &Album) -> String {
        let a = match self {
            AlbumTokens::ID => a.id.to_string(),
            AlbumTokens::Title => a.title.as_ref().unwrap_empty_string(),
            AlbumTokens::Duration => a.duration.unwrap_empty_string(),
            AlbumTokens::NumberOfTracks => a.number_of_tracks.unwrap_empty_string(),
            AlbumTokens::Explicit => match a.explicit.unwrap_or(false) {
                true => String::from("E"),
                false => String::new(),
            },
            AlbumTokens::AudioQuality => a.audio_quality.unwrap_empty_string(),
            AlbumTokens::ReleaseDate => a.release_date.as_ref().unwrap_empty_string(),
            AlbumTokens::ReleaseYear => a
                .release_date
                .as_ref()
                .unwrap_empty_string()
                .split('-')
                .next()
                .unwrap_empty_string(),
        };
        sanitize(a)
    }
}
#[derive(Clone, Copy)]
pub enum AlbumTokens {
    ID,
    Title,
    Duration,
    NumberOfTracks,
    Explicit,
    AudioQuality,
    ReleaseDate,
    ReleaseYear,
}

static TRACK_TOKEN_MAP: phf::Map<&'static str, TrackTokens> = phf_map! {
   "{track_id}" => TrackTokens::ID,
   "{track_name}" => TrackTokens::Title,
   "{track_duration}" => TrackTokens::Duration,
   "{track_num}" => TrackTokens::TrackNumber,
   "{track_volume}" => TrackTokens::VolumeNumber,
   "{track_isrc}" => TrackTokens::ISRC,
   "{track_explicit}" => TrackTokens::Explicit,
   "{track_quality}" => TrackTokens::AudioQuality,
   "{playlist_name}" => TrackTokens::PlaylistName,
   "{playlist_num}" => TrackTokens::PlaylistNumber,
};

#[derive(Clone, Copy)]
pub enum TrackTokens {
    ID,
    Title,
    Duration,
    TrackNumber,
    VolumeNumber,
    ISRC,
    Explicit,
    AudioQuality,
    PlaylistName,
    PlaylistNumber,
}

fn format_playlist_number(number: Option<usize>) -> String {
    number
        .map(|value| format!("{value:02}"))
        .unwrap_or_default()
}

fn format_track_number(number: usize) -> String {
    format!("{number:02}")
}

fn normalize_dj_playlist_month(title: &str) -> String {
    let parts: Vec<&str> = title.split(" - ").collect();
    if parts.len() != 3 || parts[0] != "DJ" || parts[1].len() != 4 {
        return title.to_string();
    }
    if !parts[1].chars().all(|character| character.is_ascii_digit()) {
        return title.to_string();
    }

    let month = match parts[2].to_ascii_lowercase().as_str() {
        "jan" => "01",
        "feb" => "02",
        "mar" => "03",
        "apr" => "04",
        "may" => "05",
        "jun" => "06",
        "jul" => "07",
        "aug" => "08",
        "sep" => "09",
        "oct" => "10",
        "nov" => "11",
        "dec" => "12",
        _ => return title.to_string(),
    };
    format!("DJ - {} - {month}", parts[1])
}

impl TokenMap<Track> for TrackTokens {
    fn token_map() -> &'static phf::Map<&'static str, Self> {
        &TRACK_TOKEN_MAP
    }

    fn get_token(self, v: &Track) -> String {
        let a = match self {
            TrackTokens::ID => v.id.to_string(),
            TrackTokens::Title => v.title.clone(),
            TrackTokens::Duration => v.duration.to_string(),
            TrackTokens::TrackNumber => format_track_number(v.track_number),
            TrackTokens::VolumeNumber => v.volume_number.to_string(),
            TrackTokens::ISRC => v.isrc.to_string(),
            TrackTokens::Explicit => match v.explicit {
                true => String::from("E"),
                false => String::new(),
            },
            TrackTokens::AudioQuality => v.audio_quality.to_string(),
            TrackTokens::PlaylistName => v
                .playlist
                .as_deref()
                .map(normalize_dj_playlist_month)
                .unwrap_or_default(),
            TrackTokens::PlaylistNumber => format_playlist_number(v.track_number_on_playlist),
        };
        sanitize(a)
    }
}

pub fn get_config() -> Result<Settings, Error> {
    let config = Config::builder()
        .set_default("audio_quality", "HI_RES")?
        .set_default("flac_output", "NATIVE")?
        .set_default("show_progress", true)?
        .set_default("include_singles", true)?
        .set_default("progress_refresh_rate", 5)?
        .set_default("login_key.device_code", "")?
        .set_default("login_key.country_code", "")?
        .set_default("login_key.session_id", "")?
        .set_default("download_cover", true)?
        .set_default("downloads", 3)?
        .set_default("workers", 1)?
        .set_default("cache_dir", get_cache_dir())?
        .set_default("login_key.access_token", "")?
        .set_default("login_key.refresh_token", "")?
        .set_default("login_key.expires_after", 0)?
        .set_default("login_key.auth_profile", "")?
        .set_default("api_key.client_id", "zU4XHVVkc2tDPo4t")?
        .set_default(
            "api_key.client_secret",
            "VJKhDFqJPqvsPVNBV6ukXTJmwlvbttP7wlMlrc72se4=",
        )?
        .set_default("download_paths.base_path", default_download_root())?
        .set_default("download_paths.artist_base_path", default_download_root())?
        .set_default("download_paths.playlist", "{playlist_name}")?
        .set_default(
            "download_paths.playlist_track",
            "{playlist_num} - {artist_name} - {track_name}",
        )?
        .set_default("download_paths.artist", "{artist_name}")?
        .set_default(
            "download_paths.album",
            "{album_name} [{album_id}] [{album_release_year}] ",
        )?
        .set_default(
            "download_paths.track",
            "{track_num} - {artist_name} - {track_name}",
        )?
        .add_source(File::new(CONFIG_FILE.as_str(), FileFormat::Toml).required(false))
        .build()?;
    let settings: Settings = config.try_deserialize()?;
    settings.save()?;

    Ok(settings)
}

#[cfg(target_os = "windows")]
fn default_download_root() -> &'static str {
    "$USERPROFILE/Music/Tidal"
}

#[cfg(not(target_os = "windows"))]
fn default_download_root() -> &'static str {
    "$HOME/Music/Tidal"
}

fn get_config_dir() -> String {
    let config_dir =
        var("XDG_CONFIG_HOME").unwrap_or_else(|_| var("HOME").unwrap_or_else(|_| "".to_string()));
    format!("{}/.config/tdl", config_dir)
}

fn get_cache_dir() -> String {
    format!("{}/cache", get_config_dir())
}

fn get_config_file() -> String {
    format!("{}/config.toml", get_config_dir())
}

lazy_static::lazy_static! {
   pub static ref CONFIG_HOME: String = get_config_dir();
   pub static ref CONFIG_FILE: String = get_config_file();
   pub static ref CONFIG: RwLock<Settings> = RwLock::new(get_config().expect("Unable to get configuration"));
}

#[cfg(test)]
mod tests {
    use super::{
        format_playlist_number, format_track_number, normalize_dj_playlist_month, ApiKey, AuthFlow,
        AuthProfile, DownloadPathSettings, FlacOutput, LoginKey, Settings, ANDROID_AUTO_PROFILE,
        ANDROID_PKCE_PROFILE,
    };
    use crate::api::models::AudioQuality;
    use std::collections::BTreeMap;

    fn settings(api_key: ApiKey) -> Settings {
        Settings {
            audio_quality: AudioQuality::Lossless,
            flac_output: FlacOutput::Native,
            show_progress: false,
            progress_refresh_rate: 5,
            include_singles: true,
            downloads: 1,
            workers: 1,
            download_cover: false,
            cache_dir: String::new(),
            download_paths: DownloadPathSettings {
                base_path: String::new(),
                artist_base_path: String::new(),
                playlist: String::new(),
                playlist_track: String::new(),
                artist: String::new(),
                album: String::new(),
                track: String::new(),
            },
            login_key: LoginKey {
                device_code: None,
                user_id: None,
                session_id: None,
                country_code: None,
                access_token: None,
                refresh_token: None,
                expires_after: None,
                auth_profile: None,
            },
            api_key,
            auth_profile: None,
            auth_profiles: BTreeMap::new(),
        }
    }

    #[test]
    fn playlist_numbers_are_zero_padded_to_two_digits() {
        assert_eq!(format_playlist_number(Some(1)), "01");
        assert_eq!(format_playlist_number(Some(9)), "09");
        assert_eq!(format_playlist_number(Some(10)), "10");
        assert_eq!(format_playlist_number(Some(119)), "119");
        assert_eq!(format_playlist_number(None), "");
    }

    #[test]
    fn album_track_numbers_are_zero_padded_to_two_digits() {
        assert_eq!(format_track_number(1), "01");
        assert_eq!(format_track_number(10), "10");
        assert_eq!(format_track_number(101), "101");
    }

    #[test]
    fn single_dj_month_names_are_normalized_for_vault_sorting() {
        assert_eq!(
            normalize_dj_playlist_month("DJ - 2025 - Oct"),
            "DJ - 2025 - 10"
        );
        assert_eq!(
            normalize_dj_playlist_month("DJ - 2023 - Apr-May"),
            "DJ - 2023 - Apr-May"
        );
        assert_eq!(
            normalize_dj_playlist_month("DJ - 2026 - 01"),
            "DJ - 2026 - 01"
        );
        assert_eq!(
            normalize_dj_playlist_month("DJ - Classics"),
            "DJ - Classics"
        );
    }

    #[test]
    fn legacy_builtin_credentials_resolve_to_android_auto() {
        let builtin = AuthProfile::android_auto();
        let config = settings(ApiKey {
            client_id: builtin.client_id.clone(),
            client_secret: builtin.client_secret.clone(),
        });
        let (name, resolved) = config.resolve_auth_profile().unwrap();
        assert_eq!(name, ANDROID_AUTO_PROFILE);
        assert_eq!(resolved, builtin);
    }

    #[test]
    fn unmatched_legacy_credentials_require_an_explicit_profile() {
        let config = settings(ApiKey {
            client_id: "custom".into(),
            client_secret: "secret".into(),
        });
        assert!(config
            .resolve_auth_profile()
            .unwrap_err()
            .to_string()
            .contains("legacy custom"));
    }

    #[test]
    fn named_custom_profile_resolves_its_own_identity() {
        let mut config = settings(ApiKey {
            client_id: "old".into(),
            client_secret: "old".into(),
        });
        let profile = AuthProfile {
            client_id: "custom".into(),
            client_secret: "secret".into(),
            user_agent: "Custom/1".into(),
            client_version: "1.0".into(),
            auth_flow: AuthFlow::Device,
            platform: Some("WEB".into()),
            device_type: Some("BROWSER".into()),
        };
        config.auth_profile = Some("mine".into());
        config.auth_profiles.insert("mine".into(), profile.clone());
        assert_eq!(
            config.resolve_auth_profile().unwrap(),
            ("mine".into(), profile)
        );
    }

    #[test]
    fn clearing_login_removes_all_client_bound_state() {
        let mut login = LoginKey {
            device_code: Some("device".into()),
            user_id: Some(1),
            session_id: Some("session".into()),
            country_code: Some("US".into()),
            access_token: Some("access".into()),
            refresh_token: Some("refresh".into()),
            expires_after: Some(42),
            auth_profile: Some("old".into()),
        };
        login.clear();
        assert!(login.device_code.is_none());
        assert!(login.user_id.is_none());
        assert!(login.session_id.is_none());
        assert!(login.country_code.is_none());
        assert!(login.access_token.is_none());
        assert!(login.refresh_token.is_none());
        assert!(login.expires_after.is_none());
        assert!(login.auth_profile.is_none());
    }

    #[test]
    fn selected_profile_serializes_before_toml_tables() {
        let builtin = AuthProfile::android_auto();
        let mut config = settings(ApiKey {
            client_id: builtin.client_id,
            client_secret: builtin.client_secret,
        });
        config.auth_profile = Some(ANDROID_AUTO_PROFILE.into());
        let serialized = toml::to_string_pretty(&config).unwrap();
        let decoded: Settings = toml::from_str(&serialized).unwrap();
        assert_eq!(decoded.auth_profile.as_deref(), Some(ANDROID_AUTO_PROFILE));
    }

    #[test]
    fn builtin_pkce_profile_resolves_with_pkce_flow() {
        let mut config = settings(ApiKey {
            client_id: "legacy".into(),
            client_secret: "legacy".into(),
        });
        config.auth_profile = Some(ANDROID_PKCE_PROFILE.into());
        let (name, profile) = config.resolve_auth_profile().unwrap();
        assert_eq!(name, ANDROID_PKCE_PROFILE);
        assert_eq!(profile.auth_flow, AuthFlow::Pkce);
        assert!(!profile.client_id.is_empty());
        assert!(!profile.client_secret.is_empty());
    }

    #[test]
    fn default_download_root_is_generic_and_app_scoped() {
        let root = super::default_download_root();
        assert!(root.ends_with("/Music/Tidal"));
    }

    #[test]
    fn native_flac_is_the_configured_default() {
        let value: FlacOutput = toml::from_str("value = 'NATIVE'")
            .and_then(|table: toml::Value| table["value"].clone().try_into())
            .unwrap();
        assert_eq!(value, FlacOutput::Native);
    }
}
