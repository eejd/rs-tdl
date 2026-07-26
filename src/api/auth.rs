use super::build_http_client;
use super::models::*;
use crate::config::{AuthProfile, CONFIG};
use anyhow::anyhow;
use anyhow::Error;
use reqwest::Client;

#[derive(Clone)]
pub struct AuthClient {
    client_id: String,
    client_secret: String,
    auth_base: String,
    http: Client,
}

impl AuthClient {
    pub fn new(profile: AuthProfile) -> Result<Self, Error> {
        Ok(Self {
            client_id: profile.client_id.clone(),
            client_secret: profile.client_secret.clone(),
            auth_base: "https://auth.tidal.com/v1/oauth2".to_string(),
            http: build_http_client(&profile)?,
        })
    }

    pub async fn get_device_code(&self) -> Result<DeviceAuthResponse, Error> {
        let data = DeviceAuthRequest {
            client_id: self.client_id.clone(),
            scope: Some("r_usr w_usr w_sub".to_string()),
            ..Default::default()
        };
        let body = serde_urlencoded::to_string(&data)?;
        let req = self
            .http
            .post(format!("{}/device_authorization", self.auth_base))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;

        if !req.status().is_success() {
            return Err(Error::msg("Failed to get device code"));
        }

        let device_key = req.json::<DeviceAuthResponse>().await?;
        Ok(device_key)
    }

    pub async fn verify_access_token(&self, access_token: &str) -> Result<bool, Error> {
        let req = self
            .http
            .get("https://api.tidal.com/v1/sessions")
            .bearer_auth(access_token)
            .send()
            .await?;
        Ok(req.status().is_success())
    }

    pub async fn get_session(&self, access_token: &str) -> Result<UserResponse, Error> {
        let session = self
            .http
            .get("https://api.tidal.com/v1/sessions")
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?
            .json::<UserResponse>()
            .await?;
        Ok(session)
    }

    pub async fn _login_access_token(
        &self,
        access_token: &str,
        user_id: Option<&str>,
    ) -> Result<(), Error> {
        let req = self
            .http
            .get(format!("{}/sessions", self.auth_base))
            .bearer_auth(access_token)
            .send()
            .await?
            .json::<UserResponse>()
            .await?;

        if let Some(uid) = user_id {
            if req.user_id.to_string() != uid {
                return Err(Error::msg("User ID mismatch"));
            }
        }

        {
            let mut config = CONFIG.write().await;
            //config.login_key.user_id = Some(req.get("userId"));
            config.login_key.country_code = Some(req.country_code);
            config.login_key.access_token = Some(access_token.to_string());
            config.save()?;
        }

        Ok(())
    }

    pub async fn refresh_access_token(
        &self,
        refresh_token: &str,
    ) -> Result<RefreshResponse, Error> {
        let data = DeviceAuthRequest {
            client_id: self.client_id.clone(),
            client_secret: Some(self.client_secret.clone()),
            refresh_token: Some(refresh_token.to_string()),
            grant_type: Some("refresh_token".to_string()),
            ..Default::default()
        };
        let body = serde_urlencoded::to_string(&data)?;

        let req = self
            .http
            .post("https://auth.tidal.com/v1/oauth2/token")
            .body(body)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        if req.status().is_success() {
            let res = req.json::<RefreshResponse>().await?;
            Ok(res)
        } else {
            Err(Error::msg("Failed to refresh access token"))
        }
    }

    pub async fn logout(&self, auth_token: String) -> Result<(), Error> {
        let req = self
            .http
            .post("https://api.tidal.com/v1/logout")
            .bearer_auth(auth_token)
            .send()
            .await?;

        if req.status() == 200 {
            Ok(())
        } else {
            Err(anyhow!("Failed to Logout"))
        }
    }

    pub async fn check_auth_status(&self, device_code: &str) -> Result<RefreshResponse, Error> {
        let data = DeviceAuthRequest {
            client_id: self.client_id.clone(),
            client_secret: Some(self.client_secret.clone()),
            device_code: Some(device_code.to_string()),
            scope: Some("r_usr w_usr w_sub".to_string()),
            grant_type: Some("urn:ietf:params:oauth:grant-type:device_code".to_string()),
            ..Default::default()
        };
        let body = serde_urlencoded::to_string(&data)?;
        let req = self
            .http
            .post(format!("{}/token", self.auth_base))
            .body(body)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        let status = req.status();
        let body = req.bytes().await?;
        if !status.is_success() {
            let oauth_error = serde_json::from_slice::<OAuthErrorResponse>(&body).ok();
            let error_name = oauth_error
                .as_ref()
                .map(|error| error.error.clone())
                .unwrap_or_else(|| "unknown_oauth_error".to_string());
            let sub_status = oauth_error
                .as_ref()
                .and_then(|error| error.sub_status)
                .map(|value| format!(" ({value})"))
                .unwrap_or_default();
            return Err(anyhow!(
                "OAuth error: {error_name}{sub_status} (HTTP {status})"
            ));
        }
        let res = serde_json::from_slice::<RefreshResponse>(&body)?;
        Ok(res)
    }

    pub async fn exchange_pkce_code(
        &self,
        code: &str,
        code_verifier: &str,
        client_unique_key: &str,
        redirect_uri: &str,
    ) -> Result<RefreshResponse, Error> {
        let data = [
            ("code", code),
            ("client_id", self.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("scope", "r_usr+w_usr+w_sub"),
            ("code_verifier", code_verifier),
            ("client_unique_key", client_unique_key),
        ];
        let response = self
            .http
            .post(format!("{}/token", self.auth_base))
            .form(&data)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            let error_name = serde_json::from_slice::<OAuthErrorResponse>(&body)
                .map(|error| error.error)
                .unwrap_or_else(|_| "unknown_oauth_error".to_string());
            return Err(anyhow!(
                "PKCE token exchange failed: {error_name} (HTTP {status})"
            ));
        }
        Ok(serde_json::from_slice::<RefreshResponse>(&body)?)
    }
}
