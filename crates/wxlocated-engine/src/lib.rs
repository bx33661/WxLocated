//! WxLocated engine: locate, decrypt and unpack WeChat mini-program packages
//! and statically reconstruct their source. No UI and no platform frameworks,
//! so it builds the same on macOS and Windows.

pub mod archive;
pub mod decompile;
pub mod export;
pub mod js;
pub mod json;
pub mod material;
pub mod overlay;
pub mod platform;
pub mod scan;
pub mod settings;
pub mod swift_json;
pub mod text;
pub mod trace;

/// Lowercase hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Streaming lowercase hex SHA-256 and byte length of a file.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<(String, u64)> {
    use std::io::Read as _;
    use sha2::{Digest, Sha256};

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                total += read as u64;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok((hex::encode(hasher.finalize()), total))
}
