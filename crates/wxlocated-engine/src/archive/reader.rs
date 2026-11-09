//! Random-access reads over a package file, transparently undoing WeChat's
//! local cache encryption (`V1MMWX`). Decrypted bytes live in memory only; no
//! intermediate package is written.
//!
//! Encrypted layout (all offsets in the raw file):
//!
//! ```text
//! 0..6        "V1MMWX"
//! 6..1030     AES-256-CBC(key = PBKDF2-HMAC-SHA1(appID, "saltiest", 1000), iv = "the iv: 16 bytes")
//!             of the first 1024 plain bytes; only the first 1023 are package data
//! 1030..      plain bytes 1023.. each XORed with the second-to-last byte of the AppID
//! ```

use std::fs::File;
use std::io;
use std::path::Path;

use aes::Aes256;
use aes::cipher::{BlockCipherDecrypt, KeyInit};

use super::ArchiveError;

const ENCRYPTED_PREFIX: &[u8; 6] = b"V1MMWX";
const ENCRYPTED_HEADER_SIZE: u64 = 6;
const ENCRYPTED_BLOCK_SIZE: usize = 1024;
const PLAIN_HEAD_SIZE: u64 = 1023;
const IV: &[u8; 16] = b"the iv: 16 bytes";
const SALT: &[u8] = b"saltiest";
const PBKDF2_ROUNDS: u32 = 1000;

pub(crate) fn is_encrypted_header(header: &[u8]) -> bool {
    header.starts_with(ENCRYPTED_PREFIX)
}

pub(crate) enum Storage {
    Plain,
    Encrypted { head: Box<[u8; ENCRYPTED_BLOCK_SIZE]>, xor_key: u8 },
}

pub(crate) struct ByteReader {
    file: File,
    storage: Storage,
    /// Size of the logical (decrypted) package.
    pub(crate) size: u64,
}

impl ByteReader {
    pub(crate) fn plain(path: &Path) -> Result<Self, ArchiveError> {
        let file = File::open(path).map_err(|_| ArchiveError::Unreadable)?;
        let size = file.metadata().map_err(|_| ArchiveError::Unreadable)?.len();
        Ok(Self { file, storage: Storage::Plain, size })
    }

    pub(crate) fn encrypted(path: &Path, app_id: &str) -> Result<Self, ArchiveError> {
        let file = File::open(path).map_err(|_| ArchiveError::Unreadable)?;
        let raw_size = file.metadata().map_err(|_| ArchiveError::Unreadable)?.len();
        if raw_size < ENCRYPTED_HEADER_SIZE + ENCRYPTED_BLOCK_SIZE as u64 + 1 {
            return Err(ArchiveError::DecryptionFailed);
        }
        if app_id.is_empty() {
            return Err(ArchiveError::MissingAppId);
        }

        let mut prefix = [0u8; 6];
        read_exact_at(&file, &mut prefix, 0)?;
        if !is_encrypted_header(&prefix) {
            return Err(ArchiveError::DecryptionFailed);
        }
        let mut head = Box::new([0u8; ENCRYPTED_BLOCK_SIZE]);
        read_exact_at(&file, head.as_mut_slice(), ENCRYPTED_HEADER_SIZE)?;
        decrypt_cbc(&derive_key(app_id), head.as_mut_slice());

        let bytes = app_id.as_bytes();
        let xor_key = if bytes.len() >= 2 { bytes[bytes.len() - 2] } else { 0x66 };
        Ok(Self { file, storage: Storage::Encrypted { head, xor_key }, size: raw_size - 7 })
    }

    pub(crate) fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, ArchiveError> {
        let end = offset.checked_add(length as u64).ok_or(ArchiveError::Unreadable)?;
        if end > self.size {
            return Err(ArchiveError::Unreadable);
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        match &self.storage {
            Storage::Plain => {
                let mut buffer = vec![0u8; length];
                read_exact_at(&self.file, &mut buffer, offset)?;
                Ok(buffer)
            }
            Storage::Encrypted { head, xor_key } => {
                let mut output = Vec::with_capacity(length);
                if offset < PLAIN_HEAD_SIZE {
                    let head_end = end.min(PLAIN_HEAD_SIZE);
                    output.extend_from_slice(&head[offset as usize..head_end as usize]);
                }
                if end > PLAIN_HEAD_SIZE {
                    let tail_start = offset.max(PLAIN_HEAD_SIZE);
                    let mut tail = vec![0u8; (end - tail_start) as usize];
                    read_exact_at(&self.file, &mut tail, tail_start + 7)?;
                    for byte in &mut tail {
                        *byte ^= xor_key;
                    }
                    output.extend_from_slice(&tail);
                }
                if output.len() != length {
                    return Err(ArchiveError::DecryptionFailed);
                }
                Ok(output)
            }
        }
    }
}

fn derive_key(app_id: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(app_id.as_bytes(), SALT, PBKDF2_ROUNDS, &mut key);
    key
}

/// AES-256-CBC decryption without padding, in place. `data` is a whole number
/// of blocks.
fn decrypt_cbc(key: &[u8; 32], data: &mut [u8]) {
    let cipher = Aes256::new(key.into());
    let mut previous = *IV;
    for chunk in data.as_chunks_mut::<16>().0 {
        let mut ciphertext = [0u8; 16];
        ciphertext.copy_from_slice(chunk);
        cipher.decrypt_block(chunk.into());
        for (byte, mask) in chunk.iter_mut().zip(previous) {
            *byte ^= mask;
        }
        previous = ciphertext;
    }
}

fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> Result<(), ArchiveError> {
    positional_read(file, buffer, offset).map_err(|_| ArchiveError::Unreadable)
}

#[cfg(unix)]
fn positional_read(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buffer, offset)
}

#[cfg(windows)]
fn positional_read(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        match file.seek_read(buffer, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                buffer = &mut buffer[read..];
                offset += read as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round trip: CBC-encrypt block by block, then the decryption loop must
    /// give the plaintext back. (Byte parity with the Swift reader on real
    /// packages is checked by `scripts/parity-archive.py`.)
    #[test]
    fn cbc_matches_reference_encryption() {
        use aes::cipher::BlockCipherEncrypt;
        let key = derive_key("wx0123456789abcdef");
        let plain: Vec<u8> = (0..64u8).collect();
        let cipher = Aes256::new((&key).into());
        let mut previous = *IV;
        let mut encrypted = Vec::new();
        for chunk in plain.as_chunks::<16>().0 {
            let mut block = [0u8; 16];
            for (index, byte) in block.iter_mut().enumerate() {
                *byte = chunk[index] ^ previous[index];
            }
            cipher.encrypt_block((&mut block).into());
            previous = block;
            encrypted.extend_from_slice(&block);
        }
        decrypt_cbc(&key, &mut encrypted);
        assert_eq!(encrypted, plain);
    }

    /// RFC 6070 PBKDF2-HMAC-SHA1 vector, truncated/extended to our length:
    /// guards against swapping in the wrong PRF.
    #[test]
    fn pbkdf2_sha1_vector() {
        let mut key = [0u8; 20];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"password", b"salt", 1, &mut key);
        assert_eq!(hex::encode(key), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
    }
}
