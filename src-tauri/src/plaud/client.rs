use std::collections::HashSet;

use serde_json::Value;

use super::auth::PlaudAuth;
use super::types::{
    base_url, browser_headers, region_from_redirect, PlaudRecording, PlaudRecordingDetail,
    PlaudUserInfo,
};

/// Maximum number of region redirects to follow for a single request. A
/// well-behaved account settles in one hop (us→eu, say). More than this means
/// Plaud is bouncing the account between regions; we stop rather than follow it
/// forever — the previous unbounded recursion overflowed the stack (a
/// deterministic crash-loop for affected accounts).
const MAX_REGION_REDIRECTS: u8 = 3;

pub struct PlaudClient {
    auth: PlaudAuth,
    region: String,
    http: reqwest::Client,
}

impl PlaudClient {
    pub fn new(auth: PlaudAuth, region: String) -> Self {
        Self {
            auth,
            region,
            http: reqwest::Client::new(),
        }
    }

    fn base_url(&self) -> String {
        base_url(&self.region)
    }

    /// Issue an authenticated GET. Extracted so a request can be replayed with a
    /// fresh token after a 401 without duplicating the header set.
    async fn send_get(&self, url: &str, token: &str) -> Result<reqwest::Response, String> {
        browser_headers(self.http.get(url))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("app-platform", "web")
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))
    }

    async fn request(&mut self, path: &str) -> Result<Value, String> {
        // Follow region redirects in a bounded loop rather than recursing, so a
        // server that bounces an account between regions cannot recurse forever
        // and overflow the stack.
        let mut redirects = 0u8;
        let mut visited_bases = HashSet::from([self.base_url()]);
        loop {
            let url = format!("{}{}", self.base_url(), path);
            let token = self.auth.get_token().await?;
            let mut res = self.send_get(&url, &token).await?;

            // A token can be rejected before its `exp` (revocation, clock drift,
            // or a refresh token we never managed to use). Force a renewal and
            // retry once rather than failing the whole sync.
            if res.status() == reqwest::StatusCode::UNAUTHORIZED {
                crate::login_log::info(
                    "got 401 from API — forcing token refresh and retrying once",
                );
                let token = self.auth.force_refresh().await?;
                res = self.send_get(&url, &token).await?;
            }

            if !res.status().is_success() {
                return Err(format!("Plaud API error: {}", res.status()));
            }

            let data: Value = res
                .json()
                .await
                .map_err(|e| format!("Invalid API response: {e}"))?;

            if data.get("status").and_then(|s| s.as_i64()) == Some(-302) {
                if let Some(domain) = data.pointer("/data/domains/api").and_then(|d| d.as_str()) {
                    match next_region_redirect(
                        &self.region,
                        domain,
                        &mut redirects,
                        &mut visited_bases,
                    ) {
                        Ok(Some(region)) => {
                            self.region = region;
                            continue;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            crate::login_log::error(&e);
                            return Err(e);
                        }
                    }
                }
            }

            return Ok(data);
        }
    }

    pub async fn list_recordings(&mut self) -> Result<Vec<PlaudRecording>, String> {
        let data = self.request("/file/simple/web").await?;
        let list = data
            .get("data_file_list")
            .or_else(|| data.get("data"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut recordings: Vec<PlaudRecording> = list
            .into_iter()
            .filter(|item| {
                !item
                    .get("is_trash")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .filter_map(|item| parse_recording(&item))
            .collect();

        // Newest first — the Plaud API doesn't guarantee an order.
        recordings.sort_by(|a, b| b.start_time.cmp(&a.start_time));

        Ok(recordings)
    }

    pub async fn get_recording(&mut self, id: &str) -> Result<PlaudRecordingDetail, String> {
        let data = self.request(&format!("/file/detail/{id}")).await?;
        let raw = data.get("data").unwrap_or(&data);

        let mut transcript = String::new();
        if let Some(items) = raw
            .get("pre_download_content_list")
            .and_then(|v| v.as_array())
        {
            for item in items {
                if let Some(content) = item.get("data_content").and_then(|c| c.as_str()) {
                    if content.len() > transcript.len() {
                        transcript = content.to_string();
                    }
                }
            }
        }

        Ok(PlaudRecordingDetail {
            id: raw
                .get("file_id")
                .or_else(|| raw.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string(),
            filename: raw
                .get("file_name")
                .or_else(|| raw.get("filename"))
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string(),
            duration: raw.get("duration").and_then(|v| v.as_i64()).unwrap_or(0),
            start_time: raw.get("start_time").and_then(|v| v.as_i64()).unwrap_or(0),
            transcript,
            serial_number: raw
                .get("serial_number")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    pub async fn get_user_info(&mut self) -> Result<PlaudUserInfo, String> {
        let data = self.request("/user/me").await?;
        let user = data
            .get("data_user")
            .or_else(|| data.get("data"))
            .unwrap_or(&data);

        Ok(PlaudUserInfo {
            email: user
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            nickname: user
                .get("nickname")
                .and_then(|v| v.as_str())
                .unwrap_or("Plaud User")
                .to_string(),
        })
    }

    pub async fn download_audio_bytes(&mut self, id: &str) -> Result<(Vec<u8>, String), String> {
        if let Some(url) = self.get_mp3_url(id).await? {
            let res = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("Download failed: {e}"))?;
            if !res.status().is_success() {
                return Err(format!("Download failed: {}", res.status()));
            }
            let bytes = res
                .bytes()
                .await
                .map_err(|e| format!("Download failed: {e}"))?;
            return Ok((bytes.to_vec(), "mp3".to_string()));
        }

        let dl_url = format!("{}/file/download/{id}", self.base_url());
        let send_dl = |token: &str| {
            browser_headers(self.http.get(&dl_url))
                .header("Authorization", format!("Bearer {token}"))
                .header("app-platform", "web")
                .send()
        };

        let token = self.auth.get_token().await?;
        let mut res = send_dl(&token)
            .await
            .map_err(|e| format!("Download failed: {e}"))?;

        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            crate::login_log::info("got 401 on download — forcing token refresh and retrying once");
            let token = self.auth.force_refresh().await?;
            res = send_dl(&token)
                .await
                .map_err(|e| format!("Download failed: {e}"))?;
        }

        if !res.status().is_success() {
            return Err(format!("Download failed: {}", res.status()));
        }

        let bytes = res
            .bytes()
            .await
            .map_err(|e| format!("Download failed: {e}"))?;
        Ok((bytes.to_vec(), "opus".to_string()))
    }

    async fn get_mp3_url(&mut self, id: &str) -> Result<Option<String>, String> {
        let data = self
            .request(&format!("/file/temp-url/{id}?is_opus=false"))
            .await?;
        Ok(data
            .get("url")
            .or_else(|| data.pointer("/data/url"))
            .or_else(|| data.get("data").filter(|v| v.is_string()))
            .or_else(|| data.get("temp_url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }
}

/// Decide whether an in-body Plaud region redirect can be followed safely.
///
/// This is deliberately separate from the HTTP request so the loop and cycle
/// guards can be tested without making live API calls.
fn next_region_redirect(
    current_region: &str,
    domain: &str,
    redirects: &mut u8,
    visited_bases: &mut HashSet<String>,
) -> Result<Option<String>, String> {
    let Some(region) = region_from_redirect(domain) else {
        return Ok(None);
    };

    let current_base = base_url(current_region);
    let next_base = base_url(&region);
    if next_base == current_base {
        return Ok(None);
    }

    if *redirects >= MAX_REGION_REDIRECTS {
        return Err(format!(
            "Plaud API region redirect limit exceeded after {MAX_REGION_REDIRECTS} hops"
        ));
    }
    if !visited_bases.insert(next_base) {
        return Err(
            "Plaud API region redirect loop detected; refusing to retry indefinitely".into(),
        );
    }

    *redirects += 1;
    Ok(Some(region))
}

fn parse_recording(item: &Value) -> Option<PlaudRecording> {
    let id = item
        .get("id")
        .or_else(|| item.get("file_id"))
        .and_then(|v| v.as_str())?
        .to_string();

    let filename = item
        .get("filename")
        .or_else(|| item.get("file_name"))
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();

    Some(PlaudRecording {
        id,
        filename,
        duration: item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0),
        start_time: item.get("start_time").and_then(|v| v.as_i64()).unwrap_or(0),
        is_trans: item
            .get("is_trans")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        serial_number: item
            .get("serial_number")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        downloaded: false,
        local_transcript: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_redirect_cycle_fails_before_retrying_forever() {
        let mut redirects = 0;
        let mut visited = HashSet::from([base_url("us")]);

        assert_eq!(
            next_region_redirect(
                "us",
                "https://api-euc1.plaud.ai",
                &mut redirects,
                &mut visited,
            )
            .unwrap(),
            Some("eu".to_string())
        );

        let error =
            next_region_redirect("eu", "https://api.plaud.ai", &mut redirects, &mut visited)
                .expect_err("a us → eu → us cycle must be rejected");
        assert!(error.contains("redirect loop"));
    }

    #[test]
    fn region_redirect_limit_fails_cleanly() {
        let mut redirects = MAX_REGION_REDIRECTS;
        let mut visited = HashSet::from([base_url("us")]);

        let error = next_region_redirect(
            "us",
            "https://api-euc1.plaud.ai",
            &mut redirects,
            &mut visited,
        )
        .expect_err("redirects beyond the limit must be rejected");
        assert!(error.contains("redirect limit"));
    }
}
