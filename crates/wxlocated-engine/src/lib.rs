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
