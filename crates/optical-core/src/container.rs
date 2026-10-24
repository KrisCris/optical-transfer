use std::io::{Read, Write};

use flate2::{Compression as FlateCompression, read::GzDecoder, write::GzEncoder};
use sha2::{Digest, Sha256};

use crate::{MAX_FILE_BYTES, OpticalError};

const FILE_HEADER_LEN: usize = 49;
const FILE_MAGIC: &[u8; 4] = b"DCF2";
const GZIP_THRESHOLD: usize = 768;
const GZIP_MINIMUM_SAVING: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
}

pub struct PackedFile {
    pub container: Vec<u8>,
    pub compression: Compression,
    pub original_size: usize,
    pub transmitted_size: usize,
}

pub struct OpticalFile {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub expected_sha256: [u8; 32],
    pub compression: Compression,
    pub transmitted_size: usize,
}

pub fn pack_file(name: &str, media_type: &str, bytes: &[u8]) -> Result<PackedFile, OpticalError> {
    if bytes.is_empty() {
        return Err(OpticalError::InvalidContainer);
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err(OpticalError::SizeLimit);
    }

    let name = safe_file_name(name);
    let media_type = if media_type.is_empty() {
        "application/octet-stream"
    } else {
        media_type
    };
    let name_bytes = name.as_bytes();
    let media_type_bytes = media_type.as_bytes();
    if name_bytes.len() > usize::from(u16::MAX) || media_type_bytes.len() > usize::from(u16::MAX) {
        return Err(OpticalError::InvalidContainer);
    }

    let compressed = if bytes.len() >= GZIP_THRESHOLD && !is_precompressed_type(media_type) {
        Some(gzip(bytes)?)
    } else {
        None
    };
    let (compression, transmitted) = match compressed {
        Some(compressed) if compressed.len() + GZIP_MINIMUM_SAVING < bytes.len() => {
            (Compression::Gzip, compressed)
        }
        _ => (Compression::None, bytes.to_vec()),
    };
    let expected_sha256: [u8; 32] = Sha256::digest(bytes).into();
    let mut container = Vec::with_capacity(
        FILE_HEADER_LEN + name_bytes.len() + media_type_bytes.len() + transmitted.len(),
    );
    container.extend_from_slice(FILE_MAGIC);
    container.push(match compression {
        Compression::None => 0,
        Compression::Gzip => 1,
    });
    container.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    container.extend_from_slice(&(media_type_bytes.len() as u16).to_le_bytes());
    container.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    container.extend_from_slice(&(transmitted.len() as u32).to_le_bytes());
    container.extend_from_slice(&expected_sha256);
    container.extend_from_slice(name_bytes);
    container.extend_from_slice(media_type_bytes);
    container.extend_from_slice(&transmitted);

    Ok(PackedFile {
        transmitted_size: transmitted.len(),
        container,
        compression,
        original_size: bytes.len(),
    })
}

/// Parse a DCF2 container into its metadata and decompressed bytes.
///
/// The returned bytes are **not verified**. This mirrors the upstream API,
/// where unpacking and integrity are deliberately separate steps: a caller must
/// run [`verify_file`] against the container's own SHA-256 -- and, at a
/// receiver, check the frame header's FNV-1a over the container first -- before
/// treating the payload as the file that was sent. `name` and `media_type`
/// likewise arrive over the optical channel and describe only what the other
/// screen claimed.
pub fn unpack_file(container: &[u8]) -> Result<OpticalFile, OpticalError> {
    if container.len() < FILE_HEADER_LEN || container[..4] != *FILE_MAGIC {
        return Err(OpticalError::InvalidContainer);
    }
    let compression = match container[4] {
        0 => Compression::None,
        1 => Compression::Gzip,
        _ => return Err(OpticalError::CompressionFailure),
    };
    let name_length = usize::from(u16::from_le_bytes([container[5], container[6]]));
    let media_type_length = usize::from(u16::from_le_bytes([container[7], container[8]]));
    let original_size =
        u32::from_le_bytes([container[9], container[10], container[11], container[12]]) as usize;
    let transmitted_size =
        u32::from_le_bytes([container[13], container[14], container[15], container[16]]) as usize;
    let data_offset = FILE_HEADER_LEN + name_length + media_type_length;
    if original_size == 0
        || original_size > MAX_FILE_BYTES
        || transmitted_size == 0
        || transmitted_size > MAX_FILE_BYTES
        || data_offset + transmitted_size != container.len()
    {
        return Err(OpticalError::InvalidContainer);
    }

    let mut expected_sha256 = [0_u8; 32];
    expected_sha256.copy_from_slice(&container[17..49]);
    let name = String::from_utf8_lossy(&container[FILE_HEADER_LEN..FILE_HEADER_LEN + name_length]);
    let media_type =
        String::from_utf8_lossy(&container[FILE_HEADER_LEN + name_length..data_offset]);
    let transmitted = &container[data_offset..];
    let bytes = match compression {
        Compression::None => transmitted.to_vec(),
        Compression::Gzip => {
            if transmitted.len() < 18 {
                return Err(OpticalError::CompressionFailure);
            }
            let trailer_start = transmitted.len() - 4;
            let trailer_size = u32::from_le_bytes(transmitted[trailer_start..].try_into().unwrap());
            if trailer_size as usize != original_size {
                return Err(OpticalError::CompressionFailure);
            }
            gunzip_bounded(transmitted, original_size)?
        }
    };
    if bytes.len() != original_size {
        return Err(OpticalError::InvalidContainer);
    }

    Ok(OpticalFile {
        name: safe_file_name(&name),
        media_type: if media_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            media_type.into_owned()
        },
        bytes,
        expected_sha256,
        compression,
        transmitted_size,
    })
}

pub fn verify_file(file: &OpticalFile) -> bool {
    let actual: [u8; 32] = Sha256::digest(&file.bytes).into();
    actual == file.expected_sha256
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, OpticalError> {
    let mut encoder = GzEncoder::new(Vec::new(), FlateCompression::default());
    encoder
        .write_all(bytes)
        .map_err(|_| OpticalError::CompressionFailure)?;
    encoder
        .finish()
        .map_err(|_| OpticalError::CompressionFailure)
}

fn gunzip_bounded(bytes: &[u8], limit: usize) -> Result<Vec<u8>, OpticalError> {
    let mut decoder = GzDecoder::new(bytes);
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|_| OpticalError::CompressionFailure)?;
        if read == 0 {
            break;
        }
        if output.len() + read > limit {
            return Err(OpticalError::SizeLimit);
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok(output)
}

pub(crate) fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|character| *character > '\u{1f}' && *character != '\u{7f}')
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "transfer.bin".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn is_precompressed_type(media_type: &str) -> bool {
    let media = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media.starts_with("video/") {
        return true;
    }
    if media.starts_with("image/") {
        return !matches!(
            media.as_str(),
            "image/bmp"
                | "image/x-ms-bmp"
                | "image/svg+xml"
                | "image/tiff"
                | "image/x-icon"
                | "image/vnd.microsoft.icon"
        );
    }
    if media.starts_with("audio/") {
        return !matches!(
            media.as_str(),
            "audio/wav"
                | "audio/x-wav"
                | "audio/wave"
                | "audio/vnd.wave"
                | "audio/aiff"
                | "audio/x-aiff"
                | "audio/basic"
                | "audio/l16"
        );
    }
    media.starts_with("application/vnd.openxmlformats-officedocument.")
        || media.starts_with("application/vnd.oasis.opendocument.")
        || media.ends_with("+zip")
        || matches!(
            media.as_str(),
            "application/gzip"
                | "application/java-archive"
                | "application/vnd.rar"
                | "application/x-7z-compressed"
                | "application/x-brotli"
                | "application/x-bzip"
                | "application/x-bzip2"
                | "application/x-gzip"
                | "application/x-lzma"
                | "application/x-rar-compressed"
                | "application/x-xz"
                | "application/x-zip-compressed"
                | "application/zip"
                | "application/zstd"
        )
}
