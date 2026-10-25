//! Platform-neutral state behind the browser sender. The UI never builds wire bytes.
use optical_core::{
    MAX_FILE_BYTES,
    bundle::{MAX_BUNDLE_FILES, SelectedFile, bundle_files},
    container::Compression,
    protocol::Wire,
    qr::{Ecc, FrameRenderer, QrFrame, compose},
    transfer::{
        FRAME_BYTES_OPTIONS, TX_FPS_OPTIONS, Transfer, TransferSettings, file_limit, human_bytes,
    },
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderInfo {
    pub name: String,
    pub file_count: usize,
    pub original_bytes: usize,
    pub transmitted_bytes: usize,
    /// Frames one clean systematic sweep takes: v3 blocks, v4 symbols.
    pub source_blocks: u32,
    pub block_len: usize,
    /// v3's carousel length; `null` on v4, which never repeats itself.
    pub cycle_frames: Option<u32>,
    pub compressed: bool,
    /// v3's session id or v4's container checksum.
    pub stream_id: u32,
    pub wire: u8,
    /// Frames per code: 1, or 3 for a colour code.
    pub layers: usize,
    pub qr_size: usize,
    pub qr_version: u8,
}

/// What the sender was asked for, as the browser's controls pass it.
#[derive(Clone, Copy, Debug)]
pub struct SendOptions<'a> {
    pub frame_bytes: usize,
    pub fps: u32,
    /// "L", "M", "Q" or "H".
    pub ecc: &'a str,
    /// v3's stream identity; v4 ignores it.
    pub session_id: u16,
    /// 4 for this build's wire, 3 for upstream's.
    pub wire: u8,
    /// Three frames per code, one per colour channel.
    pub color: bool,
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
            return Err(format!(
                "Choose fewer files: the total limit is {}, including ZIP overhead.",
                human_bytes(MAX_FILE_BYTES)
            ));
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

    pub fn prepare_files(&mut self, options: SendOptions<'_>) -> Result<SenderInfo, String> {
        let settings = settings(options)?;
        // The selection limit is v4's; a v3 stream has upstream's lower one.
        if self.total_bytes > file_limit(settings.wire) {
            return Err(format!(
                "Wire v3 carries at most {}. Choose fewer files or send as v4.",
                human_bytes(file_limit(settings.wire))
            ));
        }
        let count = self.files.len();
        let files = std::mem::take(&mut self.files);
        self.total_bytes = 0;
        let file = bundle_files(files)?;
        let transfer = Transfer::from_file(
            &file.name,
            &file.media_type,
            &file.bytes,
            settings,
            options.session_id,
        )
        .map_err(|error| error.to_string())?;
        self.activate(transfer, count)
    }

    pub fn prepare_text(
        &mut self,
        text: &str,
        options: SendOptions<'_>,
    ) -> Result<SenderInfo, String> {
        let settings = settings(options)?;
        let transfer = Transfer::from_text(text, settings, options.session_id)
            .map_err(|error| error.to_string())?;
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
            stream_id: transfer.stream_id(),
            wire: transfer.wire().version(),
            layers: transfer.settings().layers(),
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

    /// The next code on screen: one frame, or three in a colour code, as
    /// [`compose`] lays them out.
    pub fn next_code(&mut self) -> Result<Vec<u8>, String> {
        let layers = self
            .active
            .as_ref()
            .ok_or("Choose files or text and start sending first.")?
            .transfer
            .settings()
            .layers();
        let frames = (0..layers)
            .map(|_| self.next_frame())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(compose(&frames))
    }

    /// `count` codes' modules back to back, one byte per module whose bits are
    /// the dark channels (7 is black), so a browser worker crosses the
    /// WebAssembly boundary once per batch.
    pub fn next_frames(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let mut modules = Vec::new();
        for _ in 0..count {
            modules.extend(self.next_code()?);
        }
        Ok(modules)
    }
}

fn settings(options: SendOptions<'_>) -> Result<TransferSettings, String> {
    let SendOptions {
        frame_bytes,
        fps,
        ecc,
        wire,
        color,
        ..
    } = options;
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
    let wire = Wire::ALL
        .into_iter()
        .find(|candidate| candidate.version() == wire)
        .ok_or("Choose wire format v4 or v3.")?;
    Ok(TransferSettings {
        frame_bytes,
        frames_per_second: fps,
        ecc,
        grid_codes: 1,
        wire,
        color,
    })
}
