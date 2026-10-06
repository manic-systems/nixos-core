//! Admin-side helpers shared by `edit`, `rekey`, `generate` and `verify`:
//! resolving the master identity, canonical/rekeyed paths, and the recipient
//! set a canonical file must be encrypted to.

use std::path::{Path, PathBuf};

use age::{Identity, Recipient};
use anyhow::{Context, Result};

use crate::{
  config::{Config, Secret},
  identity,
};

/// Identities the admin uses to decrypt canonical secrets (files + plugin).
pub fn master_identities(cfg: &Config) -> Result<Vec<Box<dyn Identity>>> {
  identity::load_identities(
    &cfg.master.identities,
    cfg.master.plugin.as_deref(),
  )
}

/// Absolute path to a secret's canonical ciphertext.
pub fn canonical_path(secrets_dir: &Path, secret: &Secret) -> PathBuf {
  secrets_dir.join(&secret.file)
}

/// Absolute path to a secret's per-host rekeyed ciphertext.
pub fn rekeyed_path(
  secrets_dir: &Path,
  host: &str,
  secret: &Secret,
) -> Result<PathBuf> {
  let relative = secret.host_files.get(host).with_context(|| {
    format!("secret has no ciphertext path for host {host:?}")
  })?;
  Ok(secrets_dir.join(relative))
}

/// Recipients the canonical file is encrypted to. Rekeyed secrets go to the
/// master identity only; legacy secrets go to the master identity plus every
/// target host so hosts can decrypt the canonical file directly.
pub fn canonical_recipients(
  cfg: &Config,
  secret: &Secret,
) -> Result<Vec<Box<dyn Recipient + Send>>> {
  if secret.rekeyed {
    identity::build_recipients(&cfg.master.recipients)
  } else {
    let mut specs = cfg.master.recipients.clone();
    for host in &secret.hosts {
      specs.extend(cfg.host_recipients(host)?.iter().cloned());
    }
    identity::build_recipients(&specs)
  }
}
