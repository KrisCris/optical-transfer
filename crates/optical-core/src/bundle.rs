//! File-picker inputs become one unchanged file or a bounded, stored ZIP.

use std::collections::{HashMap, HashSet};

use crate::{MAX_FILE_BYTES, container::safe_file_name};

#[derive(Debug)]
pub struct SelectedFile {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    /// Browser-provided folder path, including the selected root directory.
    pub relative_path: Option<String>,
}

pub const MAX_BUNDLE_FILES: usize = 256;

/// Preserve a single selection's bytes and media type; package multiple files
/// with UTF-8 names and no compression. Folder selections always use ZIP and
/// preserve their validated relative paths. ZIP headers count toward the limit.
pub fn bundle_files(mut files: Vec<SelectedFile>) -> Result<SelectedFile, String> {
    if files.is_empty() || files.len() > MAX_BUNDLE_FILES {
        return Err(format!("Choose between 1 and {MAX_BUNDLE_FILES} files."));
    }
    for file in &mut files {
        file.name = match &file.relative_path {
            Some(path) => bundle_path(path)?,
            None => bundle_name(&file.name),
        };
        if file.name.len() > usize::from(u16::MAX) {
            return Err("A file name is too long for the transfer.".into());
        }
        if file.bytes.len() > MAX_FILE_BYTES {
            return Err("A selected file exceeds the 64 MiB limit.".into());
        }
    }
    if files.len() == 1 && files[0].relative_path.is_none() {
        return Ok(files.pop().unwrap());
    }

    // Reserve input names first so a duplicate never takes another file's name.
    let mut reserved: HashSet<String> = files.iter().map(|file| file.name.to_lowercase()).collect();
    let mut used = HashSet::new();
    let mut archive_size = 22_usize; // End of central directory.
    for file in &mut files {
        if !used.insert(file.name.to_lowercase()) {
            let (parent, leaf) = file
                .name
                .rsplit_once('/')
                .map_or((String::new(), file.name.as_str()), |(parent, leaf)| {
                    (format!("{parent}/"), leaf)
                });
            let (stem, extension) = leaf
                .rsplit_once('.')
                .filter(|(stem, _)| !stem.is_empty())
                .map_or((leaf, String::new()), |(stem, extension)| {
                    (stem, format!(".{extension}"))
                });
            for suffix in 2..=MAX_BUNDLE_FILES + 1 {
                let candidate = format!("{parent}{stem} ({suffix}){extension}");
                if reserved.insert(candidate.to_lowercase()) {
                    file.name = candidate;
                    used.insert(file.name.to_lowercase());
                    break;
                }
            }
        }
        if file.name.len() > usize::from(u16::MAX) {
            return Err("A file name is too long after resolving duplicate names.".into());
        }
        // Each file is bounded above, so this addition also fits on wasm32.
        archive_size += 30 + 46 + 2 * file.name.len() + file.bytes.len();
        if archive_size > MAX_FILE_BYTES {
            return Err("These files exceed the 64 MiB limit including ZIP headers. Choose fewer or smaller files.".into());
        }
    }

    check_path_collisions(&files)?;

    let mut bytes = Vec::with_capacity(archive_size);
    let mut directory = Vec::with_capacity(files.len());
    // PKWARE APPNOTE 4.3.7, 4.3.12 and 4.3.16: classic stored ZIP, no extras.
    // DOS timestamp is midnight 1980-01-01 (date 33), deterministic and valid.
    for file in &files {
        let offset = bytes.len() as u32;
        let mut crc = flate2::Crc::new();
        crc.update(&file.bytes);
        let crc = crc.sum();
        let size = file.bytes.len() as u32;
        bytes.extend_from_slice(b"PK\x03\x04");
        for field in [10_u16, 0x0800, 0, 0, 33] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc, size, size] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(file.name.as_bytes());
        bytes.extend_from_slice(&file.bytes);
        directory.push((offset, crc));
    }
    let central_offset = bytes.len() as u32;
    for (file, (offset, crc)) in files.iter().zip(directory) {
        bytes.extend_from_slice(b"PK\x01\x02");
        for field in [20_u16, 10, 0x0800, 0, 0, 33] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        let size = file.bytes.len() as u32;
        for field in [crc, size, size] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        for field in [file.name.len() as u16, 0, 0, 0, 0] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.extend_from_slice(&0_u32.to_le_bytes()); // External attributes.
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(file.name.as_bytes());
    }
    let central_size = bytes.len() as u32 - central_offset;
    bytes.extend_from_slice(b"PK\x05\x06");
    for field in [0_u16, 0, files.len() as u16, files.len() as u16] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    bytes.extend_from_slice(&central_size.to_le_bytes());
    bytes.extend_from_slice(&central_offset.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    debug_assert_eq!(bytes.len(), archive_size);
    Ok(SelectedFile {
        name: "transfer.zip".into(),
        media_type: "application/zip".into(),
        relative_path: None,
        bytes,
    })
}

fn bundle_path(path: &str) -> Result<String, String> {
    if path.len() > usize::from(u16::MAX) {
        return Err("A folder path is too long for the transfer.".into());
    }
    if path.contains('\\')
        || path.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || (part.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    && part.as_bytes().get(1) == Some(&b':'))
        })
    {
        return Err("Folder paths must be relative, use / separators, and contain no empty, dot, parent, or drive components.".into());
    }
    Ok(path
        .split('/')
        .map(bundle_name)
        .collect::<Vec<_>>()
        .join("/"))
}

fn check_path_collisions(files: &[SelectedFile]) -> Result<(), String> {
    // Parent IDs avoid copying every full ancestor path for deeply nested files.
    let mut directories: HashMap<(usize, String), (String, usize)> = HashMap::new();
    let mut leaves = HashSet::new();
    let mut next_parent = 1;
    for file in files {
        let mut parent = 0;
        let (clean_parent, leaf) = file.name.rsplit_once('/').unwrap_or(("", &file.name));
        if let Some((original_parent, _)) = file
            .relative_path
            .as_deref()
            .and_then(|path| path.rsplit_once('/'))
        {
            for (original, clean) in original_parent.split('/').zip(clean_parent.split('/')) {
                let key = (parent, clean.to_lowercase());
                if leaves.contains(&key) {
                    return Err(
                        "A file and folder have the same name. Rename one before sending.".into(),
                    );
                }
                let (original_name, directory_id) = directories.entry(key).or_insert_with(|| {
                    let id = next_parent;
                    next_parent += 1;
                    (original.to_owned(), id)
                });
                if original_name != original {
                    return Err("Two folder names become identical after making their paths safe. Rename one before sending.".into());
                }
                parent = *directory_id;
            }
        }
        let key = (parent, leaf.to_lowercase());
        if directories.contains_key(&key) {
            return Err("A file and folder have the same name. Rename one before sending.".into());
        }
        leaves.insert(key);
    }
    Ok(())
}

fn bundle_name(name: &str) -> String {
    let cleaned: String = safe_file_name(name)
        .chars()
        .map(|character| {
            if matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches([' ', '.']);
    if cleaned.is_empty() {
        return "transfer.bin".into();
    }
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned.into()
    }
}
