use std::{
    fs::{self, File, Permissions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use tempfile::{NamedTempFile, TempDir};

/// Roll back changes to managed files when an operation returns an error.
#[derive(Default)]
pub struct Transaction {
    changes: Vec<Change>,
    modes: Vec<(PathBuf, Permissions)>,
}

struct Change {
    path: PathBuf,
    backup: Option<TempDir>,
}

impl Transaction {
    pub fn write(&mut self, path: &Path, bytes: &[u8], mode: u32, replace: bool) -> Result<()> {
        self.put(path, mode, replace, |file| {
            file.write_all(bytes)?;
            Ok(())
        })
    }

    pub fn copy(&mut self, source: &Path, path: &Path, mode: u32) -> Result<()> {
        self.put(path, mode, false, |file| {
            io::copy(&mut File::open(source)?, file)?;
            Ok(())
        })
    }

    fn put(
        &mut self,
        path: &Path,
        mode: u32,
        replace: bool,
        write: impl FnOnce(&mut File) -> Result<()>,
    ) -> Result<()> {
        let parent = parent(path)?;
        fs::create_dir_all(parent)?;
        let mut staged = NamedTempFile::new_in(parent)?;
        write(staged.as_file_mut())?;
        staged
            .as_file()
            .set_permissions(Permissions::from_mode(mode))?;
        staged.as_file().sync_all()?;

        let exists = path.symlink_metadata().is_ok();
        if exists {
            ensure!(replace, "{} is already in use", path.display());
            ensure!(
                path.symlink_metadata()?.is_file(),
                "{} is not a regular file",
                path.display()
            );
            let backup = TempDir::new_in(parent)?;
            fs::hard_link(path, backup.path().join("original"))?;
            staged.persist(path)?;
            self.changes.push(Change {
                path: path.to_owned(),
                backup: Some(backup),
            });
        } else {
            staged.persist_noclobber(path)?;
            self.changes.push(Change {
                path: path.to_owned(),
                backup: None,
            });
        }
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) -> Result<()> {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_file(),
            "{} is not a regular file",
            path.display()
        );
        let backup = TempDir::new_in(parent(path)?)?;
        fs::rename(path, backup.path().join("original"))?;
        self.changes.push(Change {
            path: path.to_owned(),
            backup: Some(backup),
        });
        Ok(())
    }

    pub fn set_mode(&mut self, path: &Path, mode: u32) -> Result<()> {
        let original = path.metadata()?.permissions();
        fs::set_permissions(path, Permissions::from_mode(mode))?;
        self.modes.push((path.to_owned(), original));
        Ok(())
    }

    pub fn commit(mut self) {
        self.changes.clear();
        self.modes.clear();
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        for (path, permissions) in self.modes.drain(..).rev() {
            if let Err(error) = fs::set_permissions(&path, permissions) {
                eprintln!(
                    "Could not restore permissions for {} ({error})",
                    path.display()
                );
            }
        }
        for Change { path, backup } in self.changes.drain(..).rev() {
            if let Some(backup) = backup {
                if let Err(error) = fs::rename(backup.path().join("original"), &path) {
                    eprintln!(
                        "Could not restore {} ({error}). Backup kept at {}",
                        path.display(),
                        backup.keep().display()
                    );
                }
            } else if let Err(error) = fs::remove_file(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                eprintln!("Could not roll back {} ({error})", path.display());
            }
        }
    }
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent().context("File path has no parent directory")
}
