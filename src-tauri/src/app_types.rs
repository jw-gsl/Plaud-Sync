#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub region: Option<String>,
    pub name: Option<String>,
    /// Auto-sync found the session expired; the user must sign in again.
    pub needs_sign_in: bool,
}
