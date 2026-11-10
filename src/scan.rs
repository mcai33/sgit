use crate::{Repository, Result, git};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs::{self, DirEntry},
    io,
    path::PathBuf,
};

#[derive(Serialize)]
pub struct ScanResult {
    pub candidates: Vec<ScanCandidate>,
    pub skipped: usize,
}

#[derive(Serialize)]
pub struct ScanCandidate {
    pub name: String,
    pub path: PathBuf,
    pub registered: bool,
}

pub(crate) fn scan(paths: &[PathBuf], registered: &[Repository]) -> Result<ScanResult> {
    let mut roots = paths
        .iter()
        .map(|path| {
            let root = path
                .canonicalize()
                .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
            if !root.is_dir() {
                return Err(format!("Not a directory: {}", path.display()));
            }
            Ok(root)
        })
        .collect::<Result<Vec<_>>>()?;
    roots.sort_unstable();
    let mut directories: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !directories.iter().any(|parent| root.starts_with(parent)) {
            directories.push(root);
        }
    }

    let mut skipped = 0;
    let mut found = BTreeSet::new();
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if entry.file_name() == ".git" {
                match git::root(&directory) {
                    Ok(root) if root == directory => {
                        found.insert(root);
                    }
                    _ => skipped += 1,
                }
                continue;
            }
            match traversable(&entry) {
                Ok(true) => directories.push(entry.path()),
                Ok(false) => {}
                Err(_) => skipped += 1,
            }
        }
    }

    let candidates = found
        .into_iter()
        .map(|path| ScanCandidate {
            name: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            registered: registered.iter().any(|repo| repo.path == path),
            path,
        })
        .collect();
    Ok(ScanResult {
        candidates,
        skipped,
    })
}

fn traversable(entry: &DirEntry) -> io::Result<bool> {
    if !entry.file_type()?.is_dir() {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        return Ok(entry.metadata()?.file_attributes() & REPARSE_POINT == 0);
    }
    #[cfg(not(windows))]
    Ok(true)
}
