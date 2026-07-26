use clap::ArgMatches;
use std::io;
use tdl::api::auth::AuthClient;
use tdl::api::models::{Album, Artist, Track};
use tdl::cli::{cli, parse_config_flags};
use tdl::config::CONFIG;
use tdl::download::dispatch_downloads;
use tdl::download::ReceiveChannel;
use tdl::login::*;

use clap_complete::{generate, Shell};
use clap_complete_fig::Fig;

use env_logger::Env;
use futures::future::join_all;
use futures::StreamExt;

use log::debug;
use tabled::{TableIteratorExt, Tabled};
use tokio::join;
use tokio_stream::wrappers::ReceiverStream;

#[tokio::main]
async fn main() {
    // read from config to always trigger initialization of the default config if it doesn't exist
    // then release lock immediately.
    {
        let _ = CONFIG.read().await;
    }
    env_logger::Builder::from_env(Env::default().default_filter_or("none")).init();
    let matches = cli().get_matches();
    match matches.subcommand() {
        Some(("get", get_matches)) => get(get_matches).await,
        Some(("playlists", playlist_matches)) => {
            if let Err(error) = playlists(playlist_matches).await {
                eprintln!("Unable to list playlists: {error:#}");
                std::process::exit(1);
            }
        }
        Some(("search", search_matches)) => search(search_matches).await,
        Some(("login", _)) => {
            login().await;
        }
        Some(("logout", _)) => logout().await,
        Some(("autocomplete", matches)) => autocomplete(matches),
        _ => unreachable!(), // If all subcommands are defined above, anything else is unreachable!()
    }
}

#[derive(Tabled)]
struct PlaylistRow {
    #[tabled(rename = "Title")]
    title: String,
    #[tabled(rename = "Tracks")]
    tracks: usize,
    #[tabled(rename = "URL")]
    url: String,
}

async fn playlists(matches: &ArgMatches) -> Result<(), anyhow::Error> {
    let client = login().await;
    let filter = matches.get_one::<String>("filter").map(String::as_str);
    let mut playlists = client.media.get_user_playlists().await?;

    if let Some(filter) = filter {
        playlists.retain(|playlist| playlist_title_matches(&playlist.title, filter));
    }

    playlists.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.uuid.cmp(&right.uuid))
    });

    if playlists.is_empty() {
        println!("No playlists matched.");
        return Ok(());
    }

    let table = playlists
        .into_iter()
        .map(|playlist| PlaylistRow {
            title: playlist.title,
            tracks: playlist.number_of_tracks,
            url: format!("https://tidal.com/browse/playlist/{}", playlist.uuid),
        })
        .table();
    println!("{table}");
    Ok(())
}

fn playlist_title_matches(title: &str, raw_filter: &str) -> bool {
    let filter = normalize_filter(raw_filter).to_lowercase();
    let title = title.to_lowercase();
    if filter.contains(['*', '?']) {
        glob_matches(&filter, &title)
    } else {
        title.contains(&filter)
    }
}

fn normalize_filter(filter: &str) -> String {
    let mut normalized = String::with_capacity(filter.len());
    let mut chars = filter.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(next @ (' ' | '*' | '?' | '\\')) = chars.peek().copied() {
                normalized.push(next);
                chars.next();
                continue;
            }
        }
        normalized.push(character);
    }
    normalized
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;

    for token in pattern {
        let mut current = vec![false; value.len() + 1];
        match token {
            '*' => {
                current[0] = previous[0];
                for index in 1..=value.len() {
                    current[index] = previous[index] || current[index - 1];
                }
            }
            '?' => {
                current[1..].copy_from_slice(&previous[..value.len()]);
            }
            literal => {
                for index in 1..=value.len() {
                    current[index] = previous[index - 1] && value[index - 1] == literal;
                }
            }
        }
        previous = current;
    }

    previous[value.len()]
}

async fn get(matches: &ArgMatches) {
    let client = login().await;

    parse_config_flags(matches).await;
    if let Some(urls) = matches.get_many::<String>("URL") {
        let url: Vec<String> = urls.map(|i| i.to_owned()).collect();
        debug!("Collected args");
        let (handles, download, worker) = dispatch_downloads(url, client)
            .await
            .expect("Unable to dispatch download thread");
        let config = CONFIG.read().await;
        join!(
            join_all(handles),
            consume_channel(download, config.downloads.into(),),
            consume_channel(worker, config.workers.into())
        );
    }
}

async fn consume_channel(channel: ReceiveChannel, concurrency: usize) {
    //The channel receives an unexecuted future as a stream
    ReceiverStream::new(channel)
        //execute that future in a greenthread
        .map(|i| async { tokio::task::spawn(i).await })
        //up to a maximum concurrent tasks at a single time
        .buffer_unordered(concurrency)
        .for_each(|r| async {
            match r {
                Ok(l) => match l {
                    Ok(_) => {}
                    //if the task failed
                    Err(f) => eprintln!("{f:#}"),
                },
                // if we failed to launch the task
                Err(e) => eprintln!("{e:#}"),
            }
        })
        .await;
}

async fn search(matches: &ArgMatches) {
    let client = login().await;
    if let Some(query) = matches.get_one::<String>("query") {
        let max = matches.get_one::<usize>("max").cloned();
        let result = match matches.get_one::<String>("filter") {
            Some(filter) => match filter.as_str() {
                "artist" => {
                    client
                        .search
                        .search_content::<Artist>("artists", query, max)
                        .await
                }
                "track" => {
                    client
                        .search
                        .search_content::<Track>("tracks", query, max)
                        .await
                }
                "album" => {
                    client
                        .search
                        .search_content::<Album>("albums", query, max)
                        .await
                }
                _ => unreachable!(),
            },
            None => todo!(), //search all
        };
        match result {
            Ok(t) => println!("{t}"),
            Err(e) => eprintln!("{e}"),
        }
    }
}

async fn logout() {
    let config = CONFIG.read().await;
    match config.login_key.access_token.clone() {
        Some(token) => match AuthClient::new(config.api_key.clone())
            .logout(token.to_owned())
            .await
        {
            Ok(_) => println!("Logout Sucessful"),
            Err(e) => eprintln!("Error Logging out: {e}"),
        },
        None => println!("No Auth Token is configured to logout with"),
    }
}

fn autocomplete(matches: &ArgMatches) {
    let mut cmd = cli();
    if let Some(shell) = matches.get_one::<Shell>("shell") {
        generate(
            shell.to_owned(),
            &mut cmd,
            env!("CARGO_PKG_NAME"),
            &mut io::stdout(),
        )
    }

    if matches.contains_id("fig") {
        generate(Fig, &mut cmd, env!("CARGO_PKG_NAME"), &mut io::stdout())
    }
}

#[cfg(test)]
mod tests {
    use super::{glob_matches, normalize_filter, playlist_title_matches};

    #[test]
    fn playlist_filter_uses_case_insensitive_substrings() {
        assert!(playlist_title_matches("DJ - 2026 - 07", "dj -"));
        assert!(!playlist_title_matches("Road Trip", "DJ -"));
    }

    #[test]
    fn playlist_filter_uses_whole_title_globs() {
        assert!(playlist_title_matches("DJ - 2026 - 07", "DJ -*"));
        assert!(playlist_title_matches("DJ - 2026 - 07", "DJ - 202? - 07"));
        assert!(!playlist_title_matches("My DJ - 2026 - 07", "DJ -*"));
        assert!(glob_matches("*2026*", "dj - 2026 - 07"));
    }

    #[test]
    fn playlist_filter_normalizes_redundant_shell_escapes() {
        assert_eq!(normalize_filter(r"DJ\ -\*"), "DJ -*");
        assert!(playlist_title_matches("DJ - 2026 - 07", r"DJ\ -\*"));
    }
}
