use std::{io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt};

// Nonblocking open prevents a FIFO from blocking before its type can be checked.
pub(crate) fn open_regular(dir: &Dir, name: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "Only regular files without hard links are allowed"
    );
    Ok(file)
}

pub(crate) fn read_text(file: &mut File, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "File exceeds the size limit ({limit} bytes)"
    );
    String::from_utf8(bytes).context("File is not valid UTF-8")
}
