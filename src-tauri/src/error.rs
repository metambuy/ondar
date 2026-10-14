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
    /// The in-app update: no update pending when the install was asked for, or the plugin's
    /// download, signature or install failure (logged at ERROR with the cause; the message
    /// carries it too).
    #[error("{0}")]
    Updater(String),
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
            OndarError::Updater(m) => ("updater", m.as_str()),
        };
        Payload { code, message }.serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire shape the page branches on: every variant serialises to `{ code, message }`
    /// with its stable code. Fails if a code string changes or a variant is left out of
    /// `serialize`'s match (the compiler catches the omission; this pins the strings).
    #[test]
    fn every_variant_serialises_to_its_code_and_message() {
        let cases = [
            (OndarError::InvalidArgument("a".into()), "invalid_argument"),
            (OndarError::Stations("b".into()), "stations"),
            (OndarError::Updater("c".into()), "updater"),
        ];
        for (err, code) in cases {
            let v = serde_json::to_value(&err).unwrap();
            assert_eq!(v["code"], code, "{err:?}");
            assert_eq!(v["message"], err.to_string(), "{err:?}");
        }
    }
}
