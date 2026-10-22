use anyhow::Result;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt, Permissions};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(super) fn create(dir: &Dir, name: &Path, bytes: &[u8]) -> Result<()> {
    let temp = PathBuf::from(format!(".space-tmp-{}", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        let mut file = dir.open_with(&temp, &options)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // Publish the complete file without replacing an existing name.
        dir.hard_link(&temp, dir, name)?;
        dir.remove_file(&temp)?;
        dir.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
}

pub(super) fn replace(
    dir: &Dir,
    name: &Path,
    bytes: &[u8],
    permissions: Permissions,
) -> Result<()> {
    let temp = PathBuf::from(format!(".space-tmp-{}", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        let mut file = dir.open_with(&temp, &options)?;
        file.set_permissions(permissions)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        dir.rename(&temp, dir, name)?;
        dir.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
}
