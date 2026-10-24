//! Preparing a transfer without freezing the window.
//!
//! Packing a file means a gzip pass and a SHA-256 pass over every byte of it.
//! At the 64 MiB limit that is seconds of work, and on the UI thread it is
//! seconds of a frozen, unresponsive window — the operating system's "not
//! responding" overlay on a program that is working correctly. Worse, the
//! obvious manual test does not catch it: a small file packs in a few
//! milliseconds and looks perfect.
//!
//! So packing runs on its own thread and the UI polls. The channel carries the
//! finished `Transfer` because building it is part of the same expensive work:
//! `LtEncoder::new` copies the whole container into its block buffer, and the
//! capacity and ECC checks want to run against the *compressed* size, which
//! does not exist until the gzip has finished.

use std::{
    sync::mpsc::{Receiver, TryRecvError, channel},
    thread,
};

use optical_core::{
    bundle::{SelectedFile, bundle_files},
    transfer::{Transfer, TransferSettings},
};

/// What the UI should put on screen right now.
pub enum PackingState {
    /// Still working. The UI shows a preparation state and keeps repainting.
    Busy,
    Ready(Box<Transfer>),
    /// Refused, with the reason in words the UI can show as they are.
    Failed(String),
    /// The worker thread died without answering, which should not happen.
    Lost,
}

/// A packing job in flight.
///
/// Dropping this abandons the job: the worker finishes and its result goes
/// nowhere, which is the correct behaviour when a user picks a different file
/// while the first is still compressing.
pub struct PackingJob {
    receiver: Receiver<Result<Transfer, String>>,
    label: String,
}

impl PackingJob {
    /// Start packing a file's bytes. Takes ownership of them, because the whole
    /// point is that the UI thread stops touching them.
    pub fn file(
        name: String,
        media_type: String,
        bytes: Vec<u8>,
        settings: TransferSettings,
        session_id: u16,
    ) -> Self {
        let label = name.clone();
        let (sender, receiver) = channel();
        thread::spawn(move || {
            let result = Transfer::from_file(&name, &media_type, &bytes, settings, session_id)
                .map_err(|error| error.to_string());
            // A closed channel means the UI moved on. Nothing to report to.
            let _ = sender.send(result);
        });
        Self { receiver, label }
    }

    /// Start packing a selection: one file as itself, several files or a folder
    /// as one stored ZIP — the same rule the browser sender follows, so a
    /// receiver cannot tell which sender it is watching.
    pub fn files(files: Vec<SelectedFile>, settings: TransferSettings, session_id: u16) -> Self {
        let label = match files.as_slice() {
            [only] if only.relative_path.is_none() => only.name.clone(),
            _ => format!("{} files", files.len()),
        };
        let (sender, receiver) = channel();
        thread::spawn(move || {
            let result = bundle_files(files).and_then(|file| {
                Transfer::from_file(
                    &file.name,
                    &file.media_type,
                    &file.bytes,
                    settings,
                    session_id,
                )
                .map_err(|error| error.to_string())
            });
            let _ = sender.send(result);
        });
        Self { receiver, label }
    }

    /// Start packing a snippet.
    ///
    /// Text is capped at 4 MiB rather than 64, so this is far cheaper than a
    /// file — but it goes through the same path so there is one code path to
    /// reason about, not a fast one and a slow one that can diverge.
    pub fn text(text: String, settings: TransferSettings, session_id: u16) -> Self {
        let (sender, receiver) = channel();
        thread::spawn(move || {
            let result =
                Transfer::from_text(&text, settings, session_id).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        Self {
            receiver,
            label: "text snippet".to_owned(),
        }
    }

    /// What is being prepared, for the UI to name while it waits.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Non-blocking. Returns `Busy` until the worker answers.
    pub fn poll(&mut self) -> PackingState {
        match self.receiver.try_recv() {
            Ok(Ok(transfer)) => PackingState::Ready(Box::new(transfer)),
            Ok(Err(error)) => PackingState::Failed(error),
            Err(TryRecvError::Empty) => PackingState::Busy,
            Err(TryRecvError::Disconnected) => PackingState::Lost,
        }
    }
}
