use std::pin::Pin;

use crate::api::auth::AuthClient;
use crate::api::models::DeviceAuthResponse;
use crate::api::TidalClient;
use crate::config::CONFIG;
use anyhow::{anyhow, Context, Error};
use console::{measure_text_width, Emoji, Term};
use console::{pad_str, style};
use futures::Future;
use indicatif::TermLike;
use log::debug;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, Duration, Instant};

type LoginResponse = Pin<Box<dyn Future<Output = Result<TidalClient, Error>>>>;
const PKCE_REDIRECT_URI: &str = "https://tidal.com/android/login/auth";

pub async fn login() -> TidalClient {
    login_with_options(None, false)
        .await
        .unwrap_or_else(|error| panic!("Unable to authenticate: {error:#}"))
}

pub async fn login_with_options(
    requested_profile: Option<&str>,
    force: bool,
) -> Result<TidalClient, Error> {
    let (profile_name, profile) = {
        let mut config = CONFIG.write().await;
        if let Some(requested_profile) = requested_profile {
            if config.auth_profile.as_deref() != Some(requested_profile) {
                config.auth_profile = Some(requested_profile.to_string());
                config.login_key.clear();
            }
        }
        let (profile_name, profile) = config.resolve_auth_profile()?;
        let explicit_unbound_profile = config.auth_profile.is_some()
            && config.login_key.auth_profile.as_deref() != Some(profile_name.as_str());
        let wrong_profile = config
            .login_key
            .auth_profile
            .as_deref()
            .map(|bound| bound != profile_name)
            .unwrap_or(false);
        if force || explicit_unbound_profile || wrong_profile {
            config.login_key.clear();
        }
        config.save()?;
        (profile_name, profile)
    };

    let auth_flow = profile.auth_flow;
    let auth = AuthClient::new(profile)?;
    let interactive_login: LoginResponse = match auth_flow {
        crate::config::AuthFlow::Device => Box::pin(login_web(auth.clone(), profile_name.clone())),
        crate::config::AuthFlow::Pkce => Box::pin(login_pkce(auth.clone(), profile_name.clone())),
    };
    let methods: [LoginResponse; 2] = [
        Box::pin(login_config(auth.clone(), profile_name.clone())),
        interactive_login,
    ];

    for method in methods {
        match method.await {
            Ok(v) => {
                debug!("Login sucessful");
                return Ok(v);
            }
            Err(e) => eprintln!("{e:#}"),
        }
    }

    Err(anyhow!("All login methods failed"))
}

pub async fn login_pkce(client: AuthClient, profile_name: String) -> Result<TidalClient, Error> {
    let code_verifier = random_url_safe(32);
    let code_challenge = base64::encode_config(
        Sha256::digest(code_verifier.as_bytes()),
        base64::URL_SAFE_NO_PAD,
    );
    let client_unique_key = format!("{:016x}", rand::random::<u64>());
    let (_, profile) = CONFIG.read().await.resolve_auth_profile()?;
    let mut authorize_url = url::Url::parse("https://login.tidal.com/authorize")?;
    authorize_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", PKCE_REDIRECT_URI)
        .append_pair("client_id", &profile.client_id)
        .append_pair("lang", "EN")
        .append_pair("appMode", "android")
        .append_pair("client_unique_key", &client_unique_key)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("restrict_signup", "true");

    println!("Open this URL and complete TIDAL login:\n{authorize_url}\n");
    println!("Paste the final redirect URL here:");
    let redirect_url =
        tokio::task::spawn_blocking(|| console::Term::stdout().read_line()).await??;
    let code = extract_pkce_code(&redirect_url)?;
    let token = client
        .exchange_pkce_code(&code, &code_verifier, &client_unique_key, PKCE_REDIRECT_URI)
        .await?;
    let user = match token.user {
        Some(user) => user,
        None => client.get_session(&token.access_token).await?,
    };
    let timestamp = chrono::Utc::now().timestamp();
    {
        let mut config = CONFIG.write().await;
        config.login_key.device_code = None;
        config.login_key.access_token = Some(token.access_token);
        config.login_key.refresh_token = token.refresh_token;
        config.login_key.expires_after = Some(token.expires_in + timestamp);
        config.login_key.user_id = Some(user.user_id);
        config.login_key.country_code = Some(user.country_code);
        config.login_key.session_id = user.session_id;
        config.login_key.auth_profile = Some(profile_name);
        config.save()?;
    }
    TidalClient::new(&*CONFIG.read().await)
}

fn random_url_safe(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::encode_config(bytes, base64::URL_SAFE_NO_PAD)
}

fn extract_pkce_code(redirect_url: &str) -> Result<String, Error> {
    let url = url::Url::parse(redirect_url).context("invalid TIDAL PKCE redirect URL")?;
    let expected = url::Url::parse(PKCE_REDIRECT_URI)?;
    if url.scheme() != expected.scheme()
        || url.host_str() != expected.host_str()
        || url.path() != expected.path()
    {
        return Err(anyhow!(
            "TIDAL PKCE redirect URL must be the final {PKCE_REDIRECT_URI} redirect"
        ));
    }
    url.query_pairs()
        .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        .filter(|code| !code.trim().is_empty())
        .context("TIDAL PKCE redirect URL did not contain a code")
}

pub async fn login_web(client: AuthClient, profile_name: String) -> Result<TidalClient, Error> {
    let code = client.get_device_code().await?;
    let now = Instant::now();
    let prompt = show_prompt(code.clone(), now);

    while now.elapsed().as_secs() <= code.expires_in {
        let login_results = match client.check_auth_status(&code.device_code).await {
            Ok(login_results) => login_results,
            Err(error) => {
                let message = error.to_string();
                if message.contains("authorization_pending") || message.contains("slow_down") {
                    sleep(Duration::from_secs(code.interval)).await;
                    continue;
                }
                hide_prompt(prompt);
                return Err(error.context("TIDAL device authorization failed"));
            }
        };
        hide_prompt(prompt);
        let timestamp = chrono::Utc::now().timestamp();
        {
            let mut config = CONFIG.write().await;
            let user = match login_results.user {
                Some(user) => user,
                None => client.get_session(&login_results.access_token).await?,
            };
            config.login_key.device_code = Some(code.device_code);
            config.login_key.access_token = Some(login_results.access_token);
            config.login_key.refresh_token = login_results.refresh_token;
            config.login_key.expires_after = Some(login_results.expires_in + timestamp);
            config.login_key.user_id = Some(user.user_id);
            config.login_key.country_code = Some(user.country_code);
            config.login_key.session_id = user.session_id;
            config.login_key.auth_profile = Some(profile_name);
            config.save()?;
        }
        return TidalClient::new(&*CONFIG.read().await);
    }
    hide_prompt(prompt);
    Err(anyhow!(
        "Login Request timed out. Please generate a new code"
    ))
}

pub async fn login_config(client: AuthClient, profile_name: String) -> Result<TidalClient, Error> {
    let config = CONFIG.read().await;
    if let Some(access_token) = config.login_key.access_token.as_ref() {
        debug!("Attempting to validate access token");
        if client.verify_access_token(access_token).await? {
            let session = client.get_session(access_token).await?;
            drop(config);
            {
                let mut config = CONFIG.write().await;
                config.login_key.user_id = Some(session.user_id);
                config.login_key.country_code = Some(session.country_code);
                config.login_key.session_id = session.session_id;
                config.login_key.auth_profile = Some(profile_name.clone());
                config.save()?;
            }
            println!("Access Token Valid");
            return TidalClient::new(&*CONFIG.read().await);
        }
    }

    if let Some(refresh_token) = config.login_key.refresh_token.as_ref() {
        debug!("Attempting to refresh access token");
        let refresh = client.refresh_access_token(refresh_token).await?;
        drop(config); //Drop our read lock
        debug!("Access token refreshed");
        let now = chrono::Utc::now().timestamp();
        {
            let mut config = CONFIG.write().await;
            config.login_key.expires_after = Some(refresh.expires_in + now);
            config.login_key.access_token = Some(refresh.access_token.clone());
            let session = client.get_session(&refresh.access_token).await?;
            config.login_key.user_id = Some(session.user_id);
            config.login_key.country_code = Some(session.country_code);
            config.login_key.session_id = session.session_id;
            config.login_key.auth_profile = Some(profile_name);
            debug!("Attempting to save access token");
            config.save().expect("Failed to Save Access Token");
            println!("Access Token Refreshed with Refresh Token");
        }

        return TidalClient::new(&*CONFIG.read().await);
    }
    debug!("All methods failed");
    Err(Error::msg(
        "Unable to authenticate with both client and refresh token",
    ))
}

fn show_prompt(code: DeviceAuthResponse, instant: Instant) -> JoinHandle<()> {
    tokio::task::spawn(async move {
        let clocks = vec![
            "🕛", "🕐", "🕑", "🕒", "🕓", "🕔", "🕕", "🕖", "🕗", "🕘", "🕙", "🕚",
        ];
        let mut animation_index = 0;
        let term = Term::stdout();
        term.hide_cursor().ok();
        let mut interval = interval(Duration::from_millis(83));
        let login_str = fmt_login(&code.verification_uri_complete);
        let login_str_width = measure_text_width(&login_str);
        term.write_line(&login_str).ok();
        loop {
            interval.tick().await;
            // re-calc terminal size every tick
            let term_width: usize = term.width().into();
            // calculate the time left in the login prompt at the current tick
            let sec_left = code.expires_in - instant.elapsed().as_secs();

            let mut time_str = format!(
                "{} {}",
                // display the current frame of the clock spinning or fallback to empty string
                Emoji(clocks[animation_index], ""),
                fmt_time_left(sec_left)
            );

            // if our terminal is wider than our base text, right align the time
            if term_width > login_str_width {
                time_str =
                    pad_str(&time_str, term_width, console::Alignment::Right, None).to_string();
            }
            // clear the last frame and re-draw
            term.clear_line().ok();
            term.write_str(&time_str).ok();
            animation_index += 1;
            if animation_index > clocks.len() - 1 {
                animation_index = 0
            }
        }
    })
}

fn hide_prompt(task: JoinHandle<()>) {
    task.abort();
    let _ = Term::stdout().show_cursor();
}

fn fmt_login(uri: &str) -> String {
    let url = format!("https://{}", uri);
    // ANSI Hyperlink format.

    format!("Please Login to Tidal: {}", style(url).underlined().bold())
}

// formats a clickable hyperlink in a terminal
// https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
// https://en.wikipedia.org/wiki/ANSI_escape_code
fn _fmt_ansi_url(display: &str, url: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b", display, url)
}

fn fmt_time_left(sec_left: u64) -> String {
    let seconds = sec_left % 60;
    let mins = (sec_left / 60) % 60;

    format!("{}:{}", pad_zero(mins), pad_zero(seconds))
}

fn pad_zero(s: impl ToString) -> String {
    let str = s.to_string();
    if str == "0" {
        "00".to_string()
    } else {
        str
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_pkce_code, PKCE_REDIRECT_URI};

    #[test]
    fn extracts_code_from_expected_android_redirect() {
        let url = format!("{PKCE_REDIRECT_URI}?code=abc123&state=ignored");
        assert_eq!(extract_pkce_code(&url).unwrap(), "abc123");
    }

    #[test]
    fn rejects_pkce_code_from_an_unrelated_redirect() {
        let error = extract_pkce_code("https://example.com/android/login/auth?code=abc123")
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be the final"));
    }

    #[test]
    fn rejects_expected_redirect_without_a_code() {
        let error = extract_pkce_code(PKCE_REDIRECT_URI)
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not contain a code"));
    }
}
