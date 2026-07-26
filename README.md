# tdl
tdl is a rust implementation of the Python Script [Tidal-Media-Downloader](https://github.com/yaronzz/Tidal-Media-Downloader).

## Overview 

tdl offers significant performance improvements over the original python script by utilizing asynchronous multi-threaded concurrency.

### How much more performant is it?

In basic testing on my laptop without any CPU/Disk contention, tdl can typically complete the same download tasks in half the time, and 3-4x less cpu time. 

tdl: `1.44s user 2.02s system 36% cpu 9.480 total`

tidal-dl:`4.20s user 4.22s system 43% cpu 19.434 total`

In situations where there is CPU contention, or very high network throughput this difference can be as high as 20x-50x. 

## Usage

### Getting Started

To setup an auth token, run the below command, and log in via the link output to the terminal

```
tdl login
```

OAuth credentials, authorization flow, client headers, and client-specific
query parameters are treated as one profile. For playback, use the built-in
Android PKCE profile and discard tokens issued by a different flow:

```sh
tdl login --profile android-pkce --force
```

Access and refresh tokens are bound to the client that issued them. Changing
profiles therefore requires authorization again. PKCE login prints a TIDAL URL;
after signing in, paste the final `https://tidal.com/android/login/auth?...`
redirect URL back into the terminal.

To get the possible parameters for any command or sub command, run:

```
tdl --help
tdl get --help
```

### Get

Get a single item

```
tdl get <URL>
tdl get https://tidal.com/browse/album/129835816
```

Get multiple items
```
tdl get <URL1> <URL2> 
tdl get https://tidal.com/browse/album/129835816 https://tidal.com/browse/album/147102710  
```

### Playlists

List playlists created by the authenticated user:

```sh
tdl playlists
```

Filter playlist titles with a case-insensitive substring, or use `*` and `?`
for whole-title glob matching. `--match` is an alias for `--filter`.

```sh
tdl playlists --filter "DJ -"
tdl playlists --filter "DJ -*"
tdl playlists --match "DJ - 202? -*"
```

Quoting the pattern prevents the shell from expanding wildcards. Redundant
shell escapes, such as `"DJ\ -\*"`, are accepted as well.


### Autocomplete

tdl will generate an autocompletion file for various shells, that can be output to the proper autocomplete directory on your system

Example:
```
tdl autocomplete -s zsh > $FPATH/tdl.zsh
```

## Config Setup

Configs are stored in `~/.config/tdl/config.toml`, and will auto-generate with the default settings when the executable is ran. 

Fresh configurations use the normal user music directory under a dedicated
`Tidal` folder (`$HOME/Music/Tidal` on macOS and Unix-like systems). Personal
mount points and storage-server paths belong only in the local config and are
not repository defaults.

### OAuth profiles

The built-in `android-pkce` and legacy `android-auto` profiles require no
additional configuration. Device-code tokens from `android-auto` may validate
for metadata but can be rejected from playback with TIDAL sub-status `4005`, so
`android-pkce` is recommended for downloads.
Custom OAuth clients must declare their complete request identity instead of
reusing the built-in Android headers:

```toml
auth_profile = "my-client"

[auth_profiles.my-client]
client_id = "..."
client_secret = "..."
user_agent = "..."
client_version = "..."
# Include these only when they belong to the selected client family.
platform = "WEB"
device_type = "BROWSER"
```

After adding or changing a custom profile, run
`tdl login --profile my-client --force`. Existing legacy custom credentials
without request-identity fields are rejected with this instruction rather than
being silently mixed with an unrelated fingerprint.

For lossless downloads, `audio/mp4` alone does not establish quality. FLAC and
ALAC codecs are accepted as lossless; AAC-LC (`mp4a.40.2`) remains lossy and is
rejected when `LOSSLESS` was requested.

### FLAC output

Containerized FLAC is converted to a normal `.flac` file by default using the
pure-Rust Symphonia decoder and `flacenc` encoder:

```toml
flac_output = "NATIVE"
```

The conversion preserves the decoded PCM exactly, including channel layout,
sample rate, bit depth, and sample count. Track metadata and artwork are written
after conversion into the final FLAC. Native FLAC is never re-encoded, ALAC
remains ALAC/M4A, and lossy codecs remain in their source format.

To retain FLAC in its delivered MP4, Ogg, or Matroska container instead:

```toml
flac_output = "SOURCE"
```

Pure-Rust FLAC encoding is substantially faster in release builds than debug
builds.

### download_paths

The `download_paths` section in config is used to decide where files will be placed in the file system.



`base_path` will expand env variables along with shell accelerators such as `~`. This is the parent folder all files will be placed under.

Each of the following keys will create a nested folder structure in the following template:
`{base_path}/{artist}/{album}/{track}`

Playlist downloads can use their own flat folder and track templates:
`{base_path}/{playlist}/{playlist_track}`. This keeps playlist order independent of
the track's position on its original album.


Examples: 

``` toml
[download_paths]
base_path = '$HOME/Music/Tidal'
artist_base_path = '$HOME/Music/Tidal'
artist = '{artist_name}'
album = '{album_name} [{album_id}] [{album_release_year}]'
track = '{track_num} - {track_name} - {track_volume}'
playlist = '{playlist_name}'
playlist_track = '{playlist_num} - {artist_name} - {track_name}'
```
Resulting Naming path:
`/Users/username/Music/100 gecs/1000 gecs [129835816] [2019]/1 - 745 sticky - 1.flac`

You can also specify any token under any key so long as it's not a child. A track will be able to use any key from the album or artist. However an album won't be able to use a track key.

Keys can also be left blank to skip folder creation.

``` toml
[download_paths]
base_path = '$HOME/Music/Tidal'
artist_base_path = '$HOME/Music/Tidal'
artist = ''
album = '{artist_name} - {album_name} [{album_id}] [{album_release_year}]'
track = '[{track_num}] - {artist_name} - {track_name} - {track_volume}'
```

Resulting Naming path:
`/Users/username/Music/100 gecs - 1000 gecs [129835816] [2019]/[1] - 100 gecs - 745 sticky - 1.flac`

Available Keys:

Artist:

|Token | Description | Example |
| ----|-----|--|
| `{artist_name}`| Artist Name| 100 Gecs
| `{artist_id}` |  Unique ID from Tidal | 10828611

Album:
|Token | Description | Example |
| ----|-----|--|
| `{album_name}`| Album Title | 1000 Gecs |
| `{album_id}` | Unique ID from Tidal | 192059802   |
| `{album_duration}` | Duration in seconds of Album |3000 | 
| `{album_tracks}` | Number of tracks in Album | 17
| `{album_explicit}`| Shortcode if album is explicit, empty if false | E |
| `{album_quality}` | String literal of `audio_quality` | HI_RES
| `{album_release}`| YYYY-MM-DD string of album release date | 2020-07-05 |
|`{album_release_year}` | YYYY string of album release | 2020 

Track: 

|Token | Description | Example |
| ----|-----|--|
  | `{track_id}` | Unique ID from Tidal | 129835817
  | `{track_name}` | Name of Track | 745 Sticky 
  | `{track_duration}` | Track Duration in Seconds | 120
  | `{track_num}` | Number track appears on album | 7 
  | `{track_volume}` | Volume number of track, if album includes multiple discs | 1 
  | `{track_isrc}` | International Standard Recording Code of track | DEZ750500205
  | `{track_explicit}` | Shortcode if album is explicit, empty if false  | E
  | `{track_quality}` | String literal of `audio_quality` | HI_RES
  | `{playlist_name}` | Playlist title for playlist downloads | DJ - 2026 - 07
  | `{playlist_num}` | Position in the playlist, padded to at least two digits | 01


### audio_quality

- `audio_quality` 
  - Quality of downloaded tracks
  - Default:
    - `HI_RES`
  - Accepted Values:
    - `HI_RES` 
      - (24bit/96kHz MQA encoded FLAC)
    - `LOSSLESS` 
      - (1411kbps|16bit/44.1kHz FLAC/ALAC)
    - `HIGH` 
      - (320kbps AAC)
    - `LOW` 
      - (96kbps AAC)

### Concurrency

- `downloads`
    - Number of concurrent downloads. Not recommended to set higher than 8.
    - Default:
        - `3`
    - Accepted Values:
        - `1`..`10`


### download_cover

- `download_cover` 
  - Download a cover.jpg in an album folder
  - Default: 
    - `true`
  - Accepted Values: 
  - `true`
  - `false`

### Progress

- `show_progress`
  - Displays a progress bar when downloading files
  - Default: 
    - `true`
  - Accepted Values: 
  - `true`
  - `false`

- `progress_refresh_rate` 
  - Refresh rate in hz of the progress bar, if show_progress is set to true. Reduce this for lower CPU usage. 
  - Default:
    -  `5`
  - Accepted values: 
    - `1`..`255`
