//! Platform-neutral state behind the browser sender. The UI never builds wire bytes.
use optical_core::{
    MAX_FILE_BYTES,
    bundle::{MAX_BUNDLE_FILES, SelectedFile, bundle_files},
    container::Compression,
    qr::{Ecc, FrameRenderer, QrFrame},
    transfer::{FRAME_BYTES_OPTIONS, TX_FPS_OPTIONS, Transfer, TransferSettings},
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderInfo {
    pub name: String,
    pub file_count: usize,
    pub original_bytes: usize,
    pub transmitted_bytes: usize,
    pub source_blocks: u16,
    pub block_len: usize,
    pub cycle_frames: u32,
    pub compressed: bool,
    pub session_id: u16,
    pub qr_size: usize,
    pub qr_version: u8,
}

struct ActiveSender {
    transfer: Transfer,
    renderer: FrameRenderer,
    first_frame: Option<QrFrame>,
}

#[derive(Default)]
pub struct SenderState {
    files: Vec<SelectedFile>,
    total_bytes: usize,
    active: Option<ActiveSender>,
}

impl SenderState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, name: &str, media_type: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.push_file(name, media_type, None, bytes)
    }

    pub fn add_directory_file(
        &mut self,
        name: &str,
        media_type: &str,
        relative_path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        self.push_file(name, media_type, Some(relative_path), bytes)
    }

    fn push_file(
        &mut self,
        name: &str,
        media_type: &str,
        relative_path: Option<&str>,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        if self.active.is_some() {
            return Err("Stop sending before changing the selection.".into());
        }
        if self.files.len() >= MAX_BUNDLE_FILES {
            return Err(format!("Choose at most {MAX_BUNDLE_FILES} files."));
        }
        let total = self
            .total_bytes
            .checked_add(bytes.len())
            .ok_or("Selection is too large.")?;
        if total > MAX_FILE_BYTES {
            return Err(
                "Choose fewer files: the total limit is 64 MiB, including ZIP overhead.".into(),
            );
        }
        if name.len() > usize::from(u16::MAX)
            || media_type.len() > usize::from(u16::MAX)
            || relative_path.is_some_and(|path| path.len() > usize::from(u16::MAX))
        {
            return Err("A file name, path, or media type is too long.".into());
        }
        self.total_bytes = total;
        self.files.push(SelectedFile {
            name: name.into(),
            relative_path: relative_path.map(str::to_owned),
            media_type: media_type.into(),
            bytes,
        });
        Ok(())
    }

    pub fn prepare_files(
        &mut self,
        frame_bytes: usize,
        fps: u32,
        ecc: &str,
        session_id: u16,
    ) -> Result<SenderInfo, String> {
        let settings = settings(frame_bytes, fps, ecc)?;
        let count = self.files.len();
        let files = std::mem::take(&mut self.files);
        self.total_bytes = 0;
        let file = bundle_files(files)?;
        let transfer = Transfer::from_file(
            &file.name,
            &file.media_type,
            &file.bytes,
            settings,
            session_id,
        )
        .map_err(|error| error.to_string())?;
        self.activate(transfer, count)
    }

    pub fn prepare_text(
        &mut self,
        text: &str,
        frame_bytes: usize,
        fps: u32,
        ecc: &str,
        session_id: u16,
    ) -> Result<SenderInfo, String> {
        let settings = settings(frame_bytes, fps, ecc)?;
        let transfer =
            Transfer::from_text(text, settings, session_id).map_err(|error| error.to_string())?;
        self.files.clear();
        self.total_bytes = 0;
        self.activate(transfer, 0)
    }

    fn activate(
        &mut self,
        mut transfer: Transfer,
        file_count: usize,
    ) -> Result<SenderInfo, String> {
        let mut renderer = transfer.renderer();
        let first = renderer
            .render(&transfer.next_payload())
            .map_err(|error| error.to_string())?;
        let info = SenderInfo {
            name: transfer.name().to_owned(),
            file_count,
            original_bytes: transfer.original_size(),
            transmitted_bytes: transfer.transmitted_size(),
            source_blocks: transfer.source_blocks(),
            block_len: transfer.block_len(),
            cycle_frames: transfer.cycle_frames(),
            compressed: transfer.compression() == Compression::Gzip,
            session_id: transfer.session_id(),
            qr_size: first.size,
            qr_version: first.version,
        };
        self.active = Some(ActiveSender {
            transfer,
            renderer,
            first_frame: Some(first),
        });
        Ok(info)
    }

    pub fn next_frame(&mut self) -> Result<QrFrame, String> {
        let active = self
            .active
            .as_mut()
            .ok_or("Choose files or text and start sending first.")?;
        if let Some(first) = active.first_frame.take() {
            return Ok(first);
        }
        active
            .renderer
            .render(&active.transfer.next_payload())
            .map_err(|error| error.to_string())
    }

    /// `count` frames' modules back to back, one byte per module (1 is dark),
    /// so a browser worker crosses the WebAssembly boundary once per batch.
    pub fn next_frames(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let mut modules = Vec::new();
        for _ in 0..count {
            let frame = self.next_frame()?;
            modules.extend(frame.pixels.into_iter().map(u8::from));
        }
        Ok(modules)
    }
}

fn settings(frame_bytes: usize, fps: u32, ecc: &str) -> Result<TransferSettings, String> {
    if !FRAME_BYTES_OPTIONS.contains(&frame_bytes) {
        return Err("Choose a frame size from the offered settings.".into());
    }
    if !TX_FPS_OPTIONS.contains(&fps) {
        return Err("Choose a frame rate from the offered settings.".into());
    }
    let ecc = match ecc {
        "L" => Ecc::Low,
        "M" => Ecc::Medium,
        "Q" => Ecc::Quartile,
        "H" => Ecc::High,
        _ => return Err("Choose error correction L, M, Q, or H.".into()),
    };
    Ok(TransferSettings {
        frame_bytes,
        frames_per_second: fps,
        ecc,
        grid_codes: 1,
    })
}
