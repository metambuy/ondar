//! Error type returned by every Tauri command. Serialised as `{ code, message }` so the UI
//! can branch on `code` without parsing strings.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum OndarError {
    #[error("{0}")]
    InvalidArgument(String),
    /// The station directory could not answer: the network failed with nothing cached, the
    /// list came back truncated, or the cache itself failed. The message says which.
    #[error("{0}")]
    Stations(String),
}

impl From<ondar_stations::ServiceError> for OndarError {
    fn from(e: ondar_stations::ServiceError) -> Self {
        match e {
            ondar_stations::ServiceError::InvalidCountry(c) => {
                OndarError::InvalidArgument(format!("invalid country code {c:?}"))
            }
            other => OndarError::Stations(other.to_string()),
        }
    }
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
            OndarError::Stations(m) => ("stations", m.as_str()),
        };
        Payload { code, message }.serialize(serializer)
    }
}
