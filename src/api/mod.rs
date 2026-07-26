use std::sync::Arc;

use self::{
    media::MediaClient,
    models::{AudioQuality, ItemResponse},
};
use crate::config::Settings;
use anyhow::Error;
use http_cache_reqwest::{CACacheManager, Cache, CacheMode, HttpCache, HttpCacheOptions};
use log::debug;
use reqwest::Client;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use serde::de::DeserializeOwned;

pub mod auth;
pub mod media;
pub mod models;
mod search;

use search::SearchClient;

// Share reqwest client for connection pooling
lazy_static::lazy_static! {
    pub static ref CLIENT:Client = build_http_client();

}

fn build_http_client() -> Client {
    reqwest::Client::builder()
    // Match the Android client identity associated with the current OAuth
    // credentials. TIDAL can reject playback when credentials and client
    // headers describe different application families.
    .user_agent("Mozilla/5.0 (Linux; Android 12; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36")
    .default_headers({
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-tidal-client-version",
            reqwest::header::HeaderValue::from_static("2025.7.16"),
        );
        headers
    })
    .build()
    .expect("Unable to build Reqwest Client")
}

fn build_middleware_client(cache_dir: String) -> ClientWithMiddleware {
    debug!("Build Request client");

    let reqwest = build_http_client();
    let retry_policy = ExponentialBackoff {
        max_n_retries: 5,
        max_retry_interval: std::time::Duration::from_millis(1000),
        min_retry_interval: std::time::Duration::from_millis(2000),
        backoff_exponent: 2,
    };
    let cache_manager = CACacheManager {
        path: cache_dir.into(),
    };
    let cache_policy = HttpCache {
        mode: CacheMode::Default,
        manager: cache_manager,
        options: HttpCacheOptions {
            cache_options: None,
            cache_key: None,
        },
    };

    ClientBuilder::new(reqwest)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .with(Cache(cache_policy))
        .build()
}

pub struct TidalClient {
    pub search: SearchClient,
    pub media: MediaClient,
}

impl TidalClient {
    pub fn new(config: &Settings) -> Self {
        let api_client = ApiClient::new(config.clone());
        Self {
            search: SearchClient::new(api_client.clone()),
            media: MediaClient::new(api_client),
        }
    }
}

#[derive(Clone)]
pub struct ApiClient {
    country_code: (String, String),
    session_id: Option<(String, String)>,
    access_token: String,
    audio_quality: AudioQuality,
    include_singles: bool,
    api_base: String,
    http_client: ClientWithMiddleware,
    uncached_http_client: Client,
}

impl ApiClient {
    fn new(config: Settings) -> Arc<Self> {
        Arc::new(Self {
            country_code: (
                String::from("countryCode"),
                config
                    .login_key
                    .country_code
                    .expect("Country code is not set in config"),
            ),
            session_id: config
                .login_key
                .session_id
                .map(|value| (String::from("sessionId"), value)),
            access_token: config
                .login_key
                .access_token
                .expect("Access Token is not present in config"),
            http_client: build_middleware_client(config.cache_dir),
            uncached_http_client: build_http_client(),
            include_singles: config.include_singles,
            api_base: String::from("https://api.tidal.com/v1"),
            audio_quality: config.audio_quality,
        })
    }

    async fn get<'a, T>(&self, url: &'a str, query: Option<&[(String, String)]>) -> Result<T, Error>
    where
        T: DeserializeOwned + 'a,
    {
        let mut params = Vec::new();
        if let Some(query) = query {
            params.extend(query);
        }
        params.push(&self.country_code);
        if let Some(session_id) = &self.session_id {
            params.push(session_id);
        }
        let req = self
            .http_client
            .get(url)
            .bearer_auth(&self.access_token)
            .query(&params);

        let response = req.send().await?;
        let status = response.status();
        let result = response.text().await?;
        debug!("{}", result);
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "TIDAL API request to {url} returned {status}: {}",
                result.chars().take(300).collect::<String>()
            ));
        }
        let result = serde_json::from_str::<T>(&result)?;
        Ok(result)
    }

    async fn get_uncached<'a, T>(
        &self,
        url: &'a str,
        query: Option<&[(String, String)]>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned + 'a,
    {
        let mut params = Vec::new();
        if let Some(query) = query {
            params.extend(query);
        }
        params.push(&self.country_code);
        if let Some(session_id) = &self.session_id {
            params.push(session_id);
        }

        let response = self
            .uncached_http_client
            .get(url)
            .bearer_auth(&self.access_token)
            .query(&params)
            .send()
            .await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let result = response.text().await?;
        debug!("{}", result);
        if !status.is_success() {
            let retry_detail = retry_after
                .map(|value| format!("; Retry-After: {value}"))
                .unwrap_or_default();
            return Err(anyhow::anyhow!(
                "TIDAL API request to {url} returned {status}{retry_detail}: {}",
                result.chars().take(300).collect::<String>()
            ));
        }
        Ok(serde_json::from_str::<T>(&result)?)
    }

    pub async fn get_items<'a, T>(
        &self,
        url: &str,
        opts: Option<Vec<(String, String)>>,
        max: Option<usize>,
    ) -> Result<Vec<T>, Error>
    where
        T: DeserializeOwned + 'a,
    {
        let mut limit = 50;
        let mut offset = 0;
        let max = max.unwrap_or(usize::MAX);
        let mut params = vec![("limit".to_string(), limit.to_string())];
        if let Some(opt) = opts {
            params.extend(opt);
        };

        let mut result: Vec<T> = Vec::new();
        'req: loop {
            params.push(("offset".to_string(), offset.to_string()));
            let json = self.get::<ItemResponse<T>>(url, Some(&params)).await?;
            limit = json.limit;
            // the minimum between the items in the response, and the total number of items requested
            let item_limit = usize::min(json.total_number_of_items, max);
            for item in json.items {
                if result.len() >= item_limit {
                    break 'req;
                }
                result.push(item);
            }
            offset += limit;
            params.pop();
            if offset >= json.total_number_of_items {
                break 'req;
            }
        }
        Ok(result)
    }
}
