use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use backhand::{FilesystemReader, InnerNode};

use crate::{icons::Icon, library::Kind};

pub fn detect_executable(path: &Path) -> Result<Kind> {
    let metadata = path.metadata()?;
    ensure!(metadata.is_file(), "Choose a regular file");
    let mut header = [0; 20];
    File::open(path)?
        .read_exact(&mut header)
        .context("The file is too short to be a Linux executable")?;
    ensure!(header.starts_with(b"\x7fELF"), "Expected a Linux ELF file");
    if &header[8..10] == b"AI" && matches!(header[10], 1 | 2) {
        return Ok(Kind::AppImage);
    }
    ensure!(
        header[4..6] == [2, 1]
            && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
            && u16::from_le_bytes([header[18], header[19]]) == 62,
        "Expected a Linux x86-64 executable"
    );
    Ok(Kind::Native)
}

#[derive(Default)]
pub struct Metadata {
    pub name: Option<String>,
    pub version: Option<String>,
    pub icon: Option<Icon>,
}

pub fn metadata(path: &Path) -> Metadata {
    read_type2_metadata(path).unwrap_or_default()
}

fn read_type2_metadata(path: &Path) -> Option<Metadata> {
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut header = [0; 64];
    file.read_exact(&mut header).ok()?;
    if &header[8..11] != b"AI\x02" || header[5] != 1 {
        return None;
    }

    // The SquashFS starts at the end of the ELF section table in a type 2 AppImage.
    let (offset, entry_size) = match header[4] {
        1 => (
            u32::from_le_bytes(header[32..36].try_into().ok()?) as u64,
            46,
        ),
        2 => (u64::from_le_bytes(header[40..48].try_into().ok()?), 58),
        _ => return None,
    };
    let size = u16::from_le_bytes(header[entry_size..entry_size + 2].try_into().ok()?) as u64;
    let count = u16::from_le_bytes(header[entry_size + 2..entry_size + 4].try_into().ok()?) as u64;
    let offset = offset.checked_add(size * count)?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"hsqs" {
        return None;
    }
    let archive = FilesystemReader::from_reader_with_offset(file, offset).ok()?;
    let entry = archive
        .files()
        .find(|node| {
            node.fullpath.parent() == Some(Path::new("/"))
                && node
                    .fullpath
                    .extension()
                    .is_some_and(|ext| ext == "desktop")
        })
        .and_then(|node| read_member(&archive, &node.fullpath, 128 * 1024))
        .and_then(|(_, bytes)| String::from_utf8(bytes).ok());

    let mut name = None;
    let mut icon_name = None;
    let mut version = None;
    if let Some(entry) = entry {
        let mut in_main_group = false;
        for line in entry.lines() {
            if line.starts_with('[') {
                in_main_group = line == "[Desktop Entry]";
            }
            if in_main_group {
                if let Some(value) = line.strip_prefix("Name=") {
                    name = Some(value.replace("\\s", " ").replace("\\\\", "\\"));
                }
                if let Some(value) = line
                    .strip_prefix("X-AppImage-Version=")
                    .filter(|value| !value.is_empty())
                {
                    version = Some(value.to_owned());
                }
                if let Some(value) = line.strip_prefix("Icon=") {
                    icon_name = Some(value.to_owned());
                }
            }
        }
    }

    let icon = read_member(&archive, Path::new("/.DirIcon"), crate::icons::MAX_SIZE)
        .or_else(|| {
            let icon_name = icon_name.as_deref()?;
            archive
                .files()
                .filter(|node| {
                    node.fullpath
                        .file_name()
                        .is_some_and(|name| name == icon_name)
                        || node
                            .fullpath
                            .file_stem()
                            .is_some_and(|name| name == icon_name)
                })
                .find_map(|node| read_member(&archive, &node.fullpath, crate::icons::MAX_SIZE))
        })
        .and_then(|(path, bytes)| Icon::from_bytes(bytes, Some(&path)).ok());
    Some(Metadata {
        name,
        version,
        icon,
    })
}

fn read_member(archive: &FilesystemReader, path: &Path, limit: u64) -> Option<(PathBuf, Vec<u8>)> {
    let mut path = path.to_owned();
    for _ in 0..8 {
        let node = archive.files().find(|node| node.fullpath == path)?;
        match &node.inner {
            InnerNode::File(file) => {
                let mut bytes = Vec::new();
                archive
                    .file(file)
                    .reader_checked()
                    .ok()?
                    .take(limit + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                return (bytes.len() as u64 <= limit).then_some((path, bytes));
            }
            InnerNode::Symlink(link) => {
                let target = path.parent()?.join(&link.link);
                path = PathBuf::from("/");
                for component in target.components() {
                    match component {
                        Component::Normal(name) => path.push(name),
                        Component::ParentDir => {
                            path.pop();
                        }
                        _ => {}
                    }
                }
            }
            _ => return None,
        }
    }
    None
}
