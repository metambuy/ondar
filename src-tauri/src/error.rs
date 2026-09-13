//! Error type returned by every Tauri command. Serialised as `{ code, message }` so the UI
//! can branch on `code` without parsing strings.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum OndarError {
    #[error("{0}")]
    InvalidArgument(String),
}

#[derive(Serialize)]
struct Payload<'a> {
    code: &'static str,
    message: &'a str,
}

impl Serialize for OndarError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (code, message) = match self {
            OndarError::InvalidArgument(m) => ("invalid_argument", m.as_str()),
        };
        Payload { code, message }.serialize(serializer)
    }
}
