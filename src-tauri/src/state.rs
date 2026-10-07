use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::storage::Storage;

/// Current unix time in seconds (0 on the impossible pre-epoch case).
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What the login webview captured. Either a Plaud JWT detected directly, an
/// SSO payload that still needs to be exchanged via `/auth/sso-callback`, or a
/// session captured straight from the webview cookies.
pub enum BrowserLogin {
    Jwt {
        token: String,
        region: String,
    },
    /// An SSO credential captured from the login webview. `body` is the JSON
    /// payload to POST to `/auth/sso-callback` — either the exact body
    /// web.plaud.ai itself sent (so it carries the correct `sso_type` for
    /// Google, Apple, Microsoft, …) or one reconstructed from a captured
    /// `id_token`. Replaying the real body avoids guessing per-provider fields.
    Sso {
        body: String,
        region: String,
    },
    /// The login webview already holds a Plaud session — captured directly from
    /// its `pld_ut` / `pld_urt` cookies (works for any SSO provider and for an
    /// already-authenticated webview).
    SessionCookie {
        user_token: String,
        refresh_token: Option<String>,
        region: String,
    },
}

impl AppState {
    /// A sign-in just succeeded: drop the "sign in again" state and sync now.
    pub fn signed_in(&self) {
        self.needs_sign_in
            .store(false, std::sync::atomic::Ordering::Release);
        self.sync_wake.notify_one();
    }

    /// The recording currently being transcribed, if any.
    pub fn transcribing_id(&self) -> Option<String> {
        self.transcribing_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_default()
    }

    /// Run `body` only if no sync pass is in flight.
    ///
    /// Returns `Err` with a user-facing message when a pass is already running,
    /// so a manual click gets honest feedback instead of silently racing the
    /// auto-sync loop.
    pub async fn run_sync_pass<T, F, Fut>(&self, what: &str, body: F) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        if self
            .sync_running
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(format!("A sync is already running — {what} skipped it."));
        }
        struct Guard<'a>(&'a AtomicBool);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _guard = Guard(&self.sync_running);
        body().await
    }
}

pub struct AppState {
    pub storage: Mutex<Storage>,
    pub browser_login_tx: Mutex<Option<oneshot::Sender<Result<BrowserLogin, String>>>>,
    /// Unix-seconds of the last download sync (manual or auto). Seeded at launch
    /// so the "next auto-sync" countdown is valid from startup. Shared with the
    /// auto-sync loop so the countdown matches the real schedule.
    pub last_sync_epoch: AtomicI64,
    /// Single-flight guard for a sync/download pass. The auto-sync loop and a
    /// manual "Sync now" can otherwise overlap: both list the same recordings,
    /// both see the file as not-yet-on-disk, and both download it seconds
    /// apart. The loser used to double-download (and could double-enqueue for
    /// transcription, which is expensive).
    pub sync_running: AtomicBool,
    /// Prevents concurrent local ASR jobs from competing for the model's
    /// memory and CPU. The first MVP intentionally runs one job at a time.
    pub local_transcription_running: AtomicBool,
    /// Set by `cancel_local_transcription` and polled at checkpoints inside the
    /// blocking transcription worker. `Arc` so it can be cloned into the
    /// `spawn_blocking` closure, which requires a `'static` handle.
    pub local_transcription_cancelled: Arc<AtomicBool>,
    /// Id of the recording being transcribed right now. Its local files must
    /// not be renamed until the run finishes: the run writes its output
    /// against the audio path it captured at the start.
    pub transcribing_id: Mutex<Option<String>>,
    /// Set once auto-sync has seen the session is no longer valid (expired,
    /// refresh token gone, or repeated 401s); cleared on the next successful
    /// sync or sign-in. Drives the in-app banner and the one-off notification.
    pub needs_sign_in: AtomicBool,
    /// Wakes the auto-sync loop early, e.g. right after a sign-in, instead of
    /// waiting out a failure backoff that can be up to an hour long.
    pub sync_wake: tokio::sync::Notify,
    pub local_model_download_running: AtomicBool,
    pub local_model_download_cancelled: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// The guard exists because overlapping passes double-download. Prove the
    /// second one is refused rather than merely slowed down.
    #[tokio::test]
    async fn second_sync_pass_is_refused_while_first_is_in_flight() {
        let state = AppState {
            storage: Mutex::new(
                crate::storage::Storage::new(std::env::temp_dir().join("plaud-sync-state-test"))
                    .expect("storage"),
            ),
            browser_login_tx: Mutex::new(None),
            last_sync_epoch: AtomicI64::new(0),
            sync_running: AtomicBool::new(false),
            local_transcription_running: AtomicBool::new(false),
            local_transcription_cancelled: Arc::new(AtomicBool::new(false)),
            transcribing_id: Mutex::new(None),
            needs_sign_in: AtomicBool::new(false),
            sync_wake: tokio::sync::Notify::new(),
            local_model_download_running: AtomicBool::new(false),
            local_model_download_cancelled: AtomicBool::new(false),
        };

        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        // Two passes launched together: the second must be refused outright.
        let body = || {
            let concurrent = concurrent.clone();
            let peak = peak.clone();
            async move {
                let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                Ok(42u32)
            }
        };
        let (a, b) = tokio::join!(
            state.run_sync_pass("first", body),
            state.run_sync_pass("second", body)
        );

        let outcomes = [a, b];
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(outcomes.iter().filter(|r| r.is_err()).count(), 1);
        assert_eq!(peak.load(Ordering::SeqCst), 1, "passes overlapped");
        let refused = outcomes
            .iter()
            .find_map(|r| r.as_ref().err())
            .expect("one pass refused");
        assert!(refused.contains("already running"), "{refused}");

        // Guard released: a later pass runs normally.
        assert!(state
            .run_sync_pass("third", || async { Ok(7u32) })
            .await
            .is_ok());
    }
}
