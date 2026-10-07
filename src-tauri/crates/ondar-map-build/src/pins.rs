//! The inputs' pins (`pins.tsv`, shared with `scripts/fetch-natural-earth.sh`): a file whose size
//! or SHA-256 differs is refused before it is read.

use sha2::{Digest, Sha256};
use std::path::Path;

pub const PINS_TSV: &str = include_str!("../pins.tsv");

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub file: String,
    pub bytes: u64,
    pub sha256: [u8; 32],
}

pub fn parse(tsv: &str) -> Result<Vec<Pin>, String> {
    let mut out = Vec::new();
    for (n, line) in tsv.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [file, bytes, sha] = f.as_slice() else {
            return Err(format!("pins.tsv:{}: three fields expected", n + 1));
        };
        out.push(Pin {
            file: file.to_string(),
            bytes: bytes
                .parse()
                .map_err(|e| format!("pins.tsv:{}: {e}", n + 1))?,
            sha256: hex32(sha).ok_or(format!("pins.tsv:{}: bad sha256", n + 1))?,
        });
    }
    Ok(out)
}

pub fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Reads `path` and checks it against `pin`: its bytes, or why it is refused.
pub fn read_checked(path: &Path, pin: &Pin) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    if bytes.len() as u64 != pin.bytes || sha != pin.sha256 {
        return Err(format!(
            "{}: refused — {} B, sha256 {} (pinned {} B, {})",
            path.display(),
            bytes.len(),
            hex(&sha),
            pin.bytes,
            hex(&pin.sha256)
        ));
    }
    Ok(bytes)
}

/// Checks every pinned file in `dir`; the checked bytes by file name.
pub fn check_all(dir: &Path) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    let mut out = std::collections::BTreeMap::new();
    for pin in parse(PINS_TSV)? {
        let b = read_checked(&dir.join(&pin.file), &pin)?;
        out.insert(pin.file.clone(), b);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table holds 12 pins (M4a commit 3).
    #[test]
    fn the_table_has_twelve_pins() {
        let p = parse(PINS_TSV).unwrap();
        assert_eq!(p.len(), 12);
        assert!(p.iter().all(|x| x.file.starts_with("ne_10m_admin_")));
    }

    /// A temp file with one flipped byte is refused before it is read, and so is one of the
    /// wrong size; the original passes. Fails if the check is skipped (the size check alone is
    /// an equivalent mutant — the SHA covers it).
    #[test]
    fn pins_refuse() {
        let dir = std::env::temp_dir().join(format!("ondar-pins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        let body = b"natural earth, pinned".to_vec();
        std::fs::write(&path, &body).unwrap();
        let pin = Pin {
            file: "f.bin".into(),
            bytes: body.len() as u64,
            sha256: Sha256::digest(&body).into(),
        };
        assert_eq!(read_checked(&path, &pin).unwrap(), body);
        let mut flipped = body.clone();
        flipped[3] ^= 1;
        std::fs::write(&path, &flipped).unwrap();
        assert!(read_checked(&path, &pin).unwrap_err().contains("refused"));
        let mut longer = body.clone();
        longer.push(b'!');
        std::fs::write(&path, &longer).unwrap();
        assert!(read_checked(&path, &pin).unwrap_err().contains("refused"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
