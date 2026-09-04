use crate::{
    OpticalError,
    container::{OpticalFile, PackedFile, pack_file},
};

pub const MAX_SNIPPET_BYTES: usize = 4 * 1024 * 1024;
const SNIPPET_MEDIA_TYPE: &str = "application/vnd.decimen.snippet";
const SNIPPET_FILE_NAME: &str = "snippet.txt";

pub fn pack_snippet(text: &str) -> Result<PackedFile, OpticalError> {
    if text.trim().is_empty() {
        return Err(OpticalError::InvalidContainer);
    }
    if text.len() > MAX_SNIPPET_BYTES {
        return Err(OpticalError::SizeLimit);
    }
    pack_file(SNIPPET_FILE_NAME, SNIPPET_MEDIA_TYPE, text.as_bytes())
}

pub fn is_snippet(file: &OpticalFile) -> bool {
    file.media_type == SNIPPET_MEDIA_TYPE
}

pub fn snippet_text(file: &OpticalFile) -> Result<&str, OpticalError> {
    if !is_snippet(file) {
        return Err(OpticalError::InvalidContainer);
    }
    std::str::from_utf8(&file.bytes).map_err(|_| OpticalError::InvalidContainer)
}
