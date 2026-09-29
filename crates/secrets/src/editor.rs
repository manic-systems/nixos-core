//! Interactive editing: decrypt a secret to a private temporary file, launch
//! `$EDITOR`, then re-encrypt. Plaintext only ever lives in a `0600` file in a
//! `0700` temporary directory that is removed on exit.

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use zeroize::Zeroizing;

use crate::{admin, config::Config, rekey, secretio};

fn editor_command() -> String {
  std::env::var("EDITOR")
    .or_else(|_| std::env::var("VISUAL"))
    .unwrap_or_else(|_| "vi".to_string())
}

/// Keep plaintext confined to the private scratch directory for the editor's
/// lifetime; return only the updated zeroizing buffer.
fn edit_plaintext(existing: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
  let dir = tempfile::Builder::new()
    .prefix("secrets-edit-")
    .tempdir()
    .context("creating temporary edit directory")?;
  // tempdir is created 0700 by mkdtemp(3); the plaintext file is 0600.
  let scratch = dir.path().join("secret");
  secretio::write_atomic(&scratch, existing, 0o600)?;

  // Pass the scratch path as a positional argument; TMPDIR may contain quotes.
  let cmd = format!("{} \"$1\"", editor_command());
  let status = Command::new("sh")
    .arg("-c")
    .arg(&cmd)
    .arg("secrets-editor")
    .arg(&scratch)
    .status()
    .with_context(|| format!("launching editor: {cmd}"))?;
  if !status.success() {
    anyhow::bail!("editor exited with {status}; leaving secret unchanged");
  }
  Ok(Zeroizing::new(
    std::fs::read(&scratch).context("reading edited plaintext")?,
  ))
}

/// Edit secret `name`, creating it if it does not yet exist.
pub fn edit(cfg: &Config, secrets_dir: &Path, name: &str) -> Result<()> {
  let secret = cfg
    .secrets
    .get(name)
    .with_context(|| format!("unknown secret {name:?}"))?;
  let identities = admin::master_identities(cfg)?;
  let canonical = admin::canonical_path(secrets_dir, secret);

  let existing = if canonical.exists() {
    secretio::decrypt_file(&canonical, &identities)
      .with_context(|| format!("decrypting {name:?} for editing"))?
  } else {
    Zeroizing::new(Vec::new())
  };

  let updated =
    edit_plaintext(&existing).with_context(|| format!("editing {name:?}"))?;
  if *updated == *existing {
    eprintln!("{name}: unchanged");
    return Ok(());
  }

  let recipients = admin::canonical_recipients(cfg, secret)?;
  secretio::encrypt_to_file(&canonical, &updated, &recipients, false)
    .with_context(|| format!("re-encrypting {name:?}"))?;

  let n =
    rekey::rekey_secret(cfg, secrets_dir, name, secret, &identities, None)?;
  eprintln!("saved {name} ({n} host(s) rekeyed)");
  Ok(())
}
