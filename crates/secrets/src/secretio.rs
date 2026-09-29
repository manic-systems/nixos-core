//! Filesystem helpers for reading and writing age ciphertext, and for writing
//! plaintext to disk with restrictive, explicit permissions.

use std::{
  io::Write,
  os::{fd::AsFd, unix::fs::PermissionsExt},
  path::Path,
};

use age::{Identity, Recipient};
use anyhow::{Context, Result};
use zeroize::Zeroizing;

use crate::crypto;

/// Decrypt the age file at `path` with `identities`. The plaintext is returned
/// in a zeroize-on-drop buffer.
pub fn decrypt_file(
  path: &Path,
  identities: &[Box<dyn Identity>],
) -> Result<Zeroizing<Vec<u8>>> {
  let data = std::fs::read(path)
    .with_context(|| format!("reading ciphertext {}", path.display()))?;
  crypto::decrypt(&data, identities)
    .with_context(|| format!("decrypting {}", path.display()))
}

/// Encrypt `plaintext` to `recipients` and write it to `path`, creating parent
/// directories as needed. Ciphertext is world-readable (it is encrypted); the
/// caller decides on armor.
pub fn encrypt_to_file(
  path: &Path,
  plaintext: &[u8],
  recipients: &[Box<dyn Recipient + Send>],
  armor: bool,
) -> Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)
      .with_context(|| format!("creating {}", parent.display()))?;
  }
  let ciphertext = crypto::encrypt(plaintext, recipients, armor)?;
  write_atomic(path, &ciphertext, 0o644)
    .with_context(|| format!("writing ciphertext {}", path.display()))
}

/// Write `data` to `path` atomically (write to a sibling temp file, then
/// rename) with the given `mode`.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
  write_atomic_with_owner(path, data, mode, None)
}

pub fn write_atomic_with_owner(
  path: &Path,
  data: &[u8],
  mode: u32,
  owner: Option<(u32, u32)>,
) -> Result<()> {
  let parent = path
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .map(Path::to_path_buf)
    .unwrap_or_else(|| Path::new(".").to_path_buf());
  std::fs::create_dir_all(&parent)
    .with_context(|| format!("creating {}", parent.display()))?;

  let mut tmp = tempfile::Builder::new()
    .prefix(".secrets.")
    .tempfile_in(&parent)
    .with_context(|| format!("creating temp file in {}", parent.display()))?;
  tmp.write_all(data).context("writing temp file")?;
  tmp.as_file().sync_all().context("fsync temp file")?;
  if let Some((uid, gid)) = owner {
    nix::unistd::fchown(
      tmp.as_file().as_fd(),
      Some(nix::unistd::Uid::from_raw(uid)),
      Some(nix::unistd::Gid::from_raw(gid)),
    )
    .context("setting temp file ownership")?;
  }
  tmp
    .as_file()
    .set_permissions(std::fs::Permissions::from_mode(mode))
    .context("setting temp permissions")?;
  tmp
    .persist(path)
    .map_err(|e| e.error)
    .with_context(|| format!("renaming into {}", path.display()))?;
  Ok(())
}
