use crate::api::{models::*, TidalClient, CLIENT};
use crate::config::{FlacOutput, CONFIG};

use crate::models::*;
use anyhow::{anyhow, Context, Error};
use futures::Future;
use indicatif::{MultiProgress, ProgressDrawTarget};
use log::{debug, info};
use metaflac::block::PictureType::CoverFront;
use metaflac::Tag;
use mp4ameta::{Img, ImgFmt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::try_join;

use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

pub type ChannelValue = Pin<Box<dyn Future<Output = Result<bool, Error>> + Send>>;
pub type ReceiveChannel = Receiver<ChannelValue>;

pub async fn dispatch_downloads(
    urls: Vec<String>,
    client: TidalClient,
) -> Result<(Vec<JoinHandle<()>>, ReceiveChannel, ReceiveChannel), Error> {
    let config = CONFIG.read().await;
    let progress = setup_multi_progress(config.show_progress, config.progress_refresh_rate);
    let client = Arc::new(client);
    // the maximum amount of items that can be buffered by the rx channel
    // this should be equal to the total number of of work items possible at a single time
    // the actual concurrent requests will be limited by the consumer.
    let buffer_size = config.workers as usize + config.downloads as usize;
    let (dl_tx, dl_rx) = mpsc::channel(buffer_size);
    let (worker_tx, worker_rx) = mpsc::channel(config.workers as usize);

    let task = DownloadTask {
        dl_channel: dl_tx,
        worker_channel: worker_tx,
        client,
        progress,
    };
    debug!("Download Task");
    let mut handles = Vec::with_capacity(urls.len());
    // for every url supplied to the get command
    for url in urls {
        let action = match Action::from_str(&url) {
            Ok(a) => a,
            Err(_) => continue, // skip the current url if it's not valid.
        };
        let id = action.id;
        let task = task.clone();

        //spawn the download task for each URL in a new thread
        let handle = tokio::task::spawn(async move {
            let res = match action.kind {
                ActionKind::Track => {
                    let channel = task.worker_channel.clone();
                    let job = Box::pin(task.download_track(id, None));
                    match channel.send(job).await {
                        Ok(_) => Ok(true),
                        Err(_) => Err(anyhow!("Error submitting track to worker queue")),
                    }
                }
                ActionKind::Album => task.download_list(ActionKind::Album, id).await,
                ActionKind::Artist => task.download_artist(id).await,
                ActionKind::Playlist => task.download_list(ActionKind::Playlist, id).await,
            };
            match res {
                Ok(_) => {}
                Err(e) => eprintln!("{e:#}"),
            };
        });

        handles.push(handle)
    }

    Ok((handles, dl_rx, worker_rx))
}

#[derive(Clone)]
pub struct DownloadTask {
    pub progress: MultiProgress,
    pub dl_channel: Sender<ChannelValue>,
    pub worker_channel: Sender<ChannelValue>,
    pub client: Arc<TidalClient>,
}

#[derive(Clone)]
struct PlaylistContext {
    name: String,
    position: usize,
}

impl DownloadTask {
    async fn download_artist(&self, id: String) -> Result<bool, Error> {
        self.progress.println("Getting Artist Albums")?;
        let albums = self.client.media.get_artist_albums(&id).await?;
        for album in albums {
            self.download_list(ActionKind::Album, album.id.to_string())
                .await?;
        }
        Ok(true)
    }

    async fn download_list(&self, kind: ActionKind, id: String) -> Result<bool, Error> {
        let url = format!("https://api.tidal.com/v1/{kind}s/{id}/items",);
        let playlist_name = match kind {
            ActionKind::Playlist => Some(self.client.media.get_playlist(&id).await?.title),
            _ => None,
        };
        let tracks = self
            .client
            .media
            .get_items::<ItemResponseItem<Track>>(&url, None, None)
            .await?;
        for (index, track) in tracks.into_iter().enumerate() {
            let playlist = playlist_name.as_ref().map(|name| PlaylistContext {
                name: name.clone(),
                position: index + 1,
            });
            let future = Box::pin(
                self.clone()
                    .download_track(track.item.id.to_string(), playlist),
            );
            match self.clone().worker_channel.send(future).await {
                Ok(_) => continue,
                Err(_) => return Err(anyhow!("Error Submitting download_track")),
            }
        }
        Ok(true)
    }

    async fn download_track(
        self,
        id: String,
        playlist: Option<PlaylistContext>,
    ) -> Result<bool, Error> {
        let mut track = self.client.media.get_track(&id).await?;
        if let Some(playlist) = playlist {
            track.playlist = Some(playlist.name);
            track.track_number_on_playlist = Some(playlist.position);
        }
        let path_str = self.get_path(&track).await?;
        let progress = self.progress.clone();
        let dl_channel = self.dl_channel.clone();
        let download = Box::pin(async move {
            let result = self.download_file(track, path_str).await;
            if let Err(error) = &result {
                let _ = progress.println(format!("Download Failed | {error:#}"));
            }
            result
        });
        match &dl_channel.send(download).await {
            Ok(_) => Ok(true),
            Err(_) => Err(anyhow!("Submitting Download Task failed")),
        }
    }

    async fn download_file(self, track: Track, mut path: PathBuf) -> Result<bool, anyhow::Error> {
        let info = track.get_info();
        let pb = ProgressBar::new(self.progress.clone(), track.id);
        let (destination_root, flac_output) = {
            let config = CONFIG.read().await;
            (
                config
                    .download_paths
                    .get_destination_root(track.playlist.is_some())?,
                config.flac_output,
            )
        };
        if !destination_root.is_dir() {
            return Err(anyhow!(
                "{info} | configured destination root is unavailable: {}. Mount or create the intended root before downloading; tdl will not create it automatically",
                destination_root.display()
            ));
        }
        let playback_manifest = self
            .client
            .media
            .get_stream_url(track.id)
            .await
            .with_context(|| format!("{info} | unable to obtain requested stream"))?;
        let source_format = playback_manifest.audio_format()?;
        let extract_native_flac =
            source_format == AudioFormat::ContainerFlac && flac_output == FlacOutput::Native;
        path.set_extension(if extract_native_flac {
            "flac"
        } else {
            playback_manifest
                .get_file_extension()
                .expect("Unable to determine track file extension")
        });

        if path.exists() {
            debug!("Path exists");
            self.progress
                .println(format!("File Exists | {}", track.get_info()))?;
            // Exit early if the file already exists
            return Ok(false);
        }

        if playback_manifest.urls.is_empty() {
            return Err(anyhow!("Playback manifest did not contain media URLs"));
        }
        let segment_count = playback_manifest.urls.len();
        if segment_count > 1 {
            pb.start_segmented_download(segment_count, &track);
        }
        tokio::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow!("Parent Directory missing somehow"))?,
        )
        .await?;
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("download");
        let source_extension = playback_manifest
            .get_file_extension()
            .expect("Unable to determine source file extension");
        let source_partial_path = path.with_file_name(format!("{stem}.part.{source_extension}"));
        let file = File::create(source_partial_path.clone()).await?;
        // 1 MiB Write buffer to minimize syscalls for slow i/o
        // Reduces write CPU time from 24% to 7%.
        let mut writer = tokio::io::BufWriter::with_capacity(1024 * 1000 * 1000, file);
        let mut downloaded: u64 = 0;
        for (index, stream_url) in playback_manifest.urls.iter().enumerate() {
            let response = CLIENT
                .get(stream_url)
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("{info} | media segment request failed"))?;
            if segment_count == 1 {
                pb.start_download(response.content_length().unwrap_or(0), &track);
            } else {
                pb.set_segment(index + 1, segment_count, &track);
            }
            let mut stream = response.bytes_stream();
            while let Some(item) = stream.next().await {
                let chunk = item?;
                downloaded += chunk.len() as u64;
                pb.set_position(downloaded);
                writer.write_all(&chunk).await?;
            }
        }

        //flush buffer to disk;
        pb.set_message(format!("Writing to Disk | {info}"));
        writer.flush().await?;
        writer.into_inner().sync_all().await?;

        let (metadata_path, output_format) = if extract_native_flac {
            pb.set_message(format!("Extracting native FLAC | {info}"));
            let input = source_partial_path.clone();
            let output = path.with_file_name(format!("{stem}.part.flac"));
            let conversion_output = output.clone();
            tokio::task::spawn_blocking(move || {
                crate::audio::convert_containerized_flac(&input, &conversion_output)
            })
            .await??;
            tokio::fs::remove_file(&source_partial_path).await?;
            (output, AudioFormat::Flac)
        } else {
            (source_partial_path, source_format)
        };

        pb.set_message(format!("Writing metadata | {info}"));
        self.write_metadata(track, metadata_path.clone(), output_format)
            .await
            .with_context(|| {
                format!(
                    "{info} | metadata write failed for {}",
                    metadata_path.display()
                )
            })?;
        tokio::fs::rename(&metadata_path, &path)
            .await
            .with_context(|| format!("{info} | unable to finalize {}", path.display()))?;
        pb.println(format!("Download Complete | {info}"));

        Ok(true)
    }

    async fn write_metadata(
        &self,
        track: Track,
        path: PathBuf,
        audio_format: AudioFormat,
    ) -> Result<(), Error> {
        let cover = if let Some(cover_id) = &track.album.cover {
            let download_cover = CONFIG.read().await.download_cover;
            Some(
                self.get_cover_data(path.clone(), cover_id, download_cover)
                    .await?,
            )
        } else {
            None
        };

        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            match audio_format {
                AudioFormat::Flac => {
                    let mut tag = Tag::read_from_path(&path)?;
                    tag.set_vorbis("TITLE", vec![track.title]);
                    tag.set_vorbis("TRACKNUMBER", vec![track.track_number.to_string()]);
                    tag.set_vorbis("ARTIST", vec![track.artist.name]);
                    tag.set_vorbis("ALBUM", vec![track.album.title.unwrap_or_default()]);
                    tag.set_vorbis("COPYRIGHT", vec![track.copyright]);
                    tag.set_vorbis("ISRC", vec![track.isrc]);
                    if let Some(cover) = cover {
                        tag.add_picture(cover.content_type, CoverFront, cover.data);
                    }
                    tag.save()?;
                }
                AudioFormat::ContainerFlac | AudioFormat::Mp4Alac | AudioFormat::Mp4Aac => {
                    let mut tag = mp4ameta::Tag::read_from_path(&path)?;
                    tag.set_title(track.title);
                    tag.set_track_number(
                        u16::try_from(track.track_number)
                            .context("track number is too large for MP4 metadata")?,
                    );
                    tag.set_artist(track.artist.name);
                    tag.set_album(track.album.title.unwrap_or_default());
                    tag.set_copyright(track.copyright);
                    tag.set_isrc(track.isrc);
                    if let Some(cover) = cover {
                        let format = match cover.content_type.as_str() {
                            "image/png" => ImgFmt::Png,
                            "image/bmp" => ImgFmt::Bmp,
                            _ => ImgFmt::Jpeg,
                        };
                        tag.set_artwork(Img::new(format, cover.data));
                    }
                    tag.write_to_path(&path)?;
                }
            }
            Ok(())
        })
        .await??;
        info!("Metadata written to file");
        Ok(())
    }

    pub async fn get_cover_data(
        &self,
        path: PathBuf,
        cover_id: &str,
        download_cover: bool,
    ) -> Result<Cover, Error> {
        let dl_path = Path::new(&path)
            .parent()
            .ok_or_else(|| anyhow!("Parent Directory Doesn't exist"))?
            .join("cover.jpg");
        let pic = self.client.media.get_cover_data(cover_id).await?;
        if download_cover {
            tokio::fs::write(dl_path, pic.data.clone()).await?;
            info!("Write cover to disk");
        }
        Ok(pic)
    }

    async fn get_path(&self, track: &Track) -> Result<PathBuf, Error> {
        let config = &CONFIG.read().await;
        let dl_path = &config.download_paths;
        let album_id = &track.album.id;
        // The track artist can be different than the album artist
        // important to use the album artist for naming.
        // prefer to use that, otherwise default to the track artist
        let artist_id = match track.album.artist.clone() {
            Some(val) => val.id.to_string(),
            None => track.artist.id.to_string(),
        };
        let (album, artist) = try_join!(
            self.client.media.get_album(*album_id),
            self.client.media.get_artist(&artist_id)
        )?;

        let path_artist = if track.playlist.is_some() {
            track.artist.clone()
        } else {
            artist
        };
        dl_path.get_track_path(track.clone(), album, path_artist)
    }
}

fn setup_multi_progress(show_progress: bool, refresh_rate: u8) -> MultiProgress {
    let mp = MultiProgress::new();
    let draw_target = match show_progress {
        true => ProgressDrawTarget::stdout_with_hz(refresh_rate),
        false => ProgressDrawTarget::hidden(),
    };
    mp.set_draw_target(draw_target);
    mp
}
