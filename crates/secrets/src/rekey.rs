//! Rekeying: re-encrypt each canonical (master-encrypted) secret to the public
//! keys of every host that needs it, using the configured host storage path.

use std::path::Path;

use age::{Identity, Recipient};
use anyhow::{Context, Result};

use crate::{
  admin,
  config::{Config, Secret},
  identity,
  secretio,
};

/// Rekey a single secret to all of its hosts (optionally filtered to one host).
pub fn rekey_secret(
  cfg: &Config,
  secrets_dir: &Path,
  name: &str,
  secret: &Secret,
  identities: &[Box<dyn Identity>],
  host_filter: Option<&str>,
) -> Result<usize> {
  if !secret.rekeyed {
    return Ok(0);
  }
  // When filtering to one host, skip secrets it does not receive so we do not
  // decrypt (and, with a YubiKey master, prompt for) irrelevant secrets.
  if let Some(only) = host_filter
    && !secret.hosts.iter().any(|host| host == only)
  {
    return Ok(0);
  }
  let canonical = admin::canonical_path(secrets_dir, secret);
  if !canonical.exists() {
    anyhow::bail!(
      "canonical secret {name:?} is missing at {}; create it with `secrets edit`",
      canonical.display()
    );
  }
  let plaintext = secretio::decrypt_file(&canonical, identities)
    .with_context(|| format!("decrypting canonical secret {name:?}"))?;

  let mut count = 0;
  for host in &secret.hosts {
    if let Some(only) = host_filter
      && only != host
    {
      continue;
    }
    let recipients = identity::build_recipients(cfg.host_recipients(host)?)?;
    let out = admin::rekeyed_path(secrets_dir, host, secret)?;
    secretio::encrypt_to_file(&out, &plaintext, &recipients, false)
      .with_context(|| format!("rekeying {name:?} for host {host:?}"))?;
    count += 1;
  }
  Ok(count)
}

/// Rekey every rekeyed secret. Returns the number of `(secret, host)` artifacts
/// written.
pub fn rekey_all(
  cfg: &Config,
  secrets_dir: &Path,
  host_filter: Option<&str>,
) -> Result<usize> {
  if let Some(host) = host_filter {
    cfg.host_recipients(host)?;
  }
  let identities = admin::master_identities(cfg)?;
  let mut total = 0;
  for (name, secret) in &cfg.secrets {
    total +=
      rekey_secret(cfg, secrets_dir, name, secret, &identities, host_filter)?;
  }
  Ok(total)
}

fn rekey_master_secret(
  cfg: &Config,
  secrets_dir: &Path,
  name: &str,
  secret: &Secret,
  identities: &[Box<dyn Identity>],
  master_recipients: &[Box<dyn Recipient + Send>],
) -> Result<()> {
  let canonical = admin::canonical_path(secrets_dir, secret);
  if !canonical.exists() {
    anyhow::bail!(
      "canonical secret {name:?} is missing at {}; create it with `secrets edit`",
      canonical.display()
    );
  }

  let plaintext = secretio::decrypt_file(&canonical, identities)
    .with_context(|| format!("decrypting canonical secret {name:?}"))?;
  let legacy_recipients = if secret.rekeyed {
    None
  } else {
    Some(admin::canonical_recipients(cfg, secret)?)
  };
  let recipients = legacy_recipients.as_deref().unwrap_or(master_recipients);
  secretio::encrypt_to_file(&canonical, &plaintext, recipients, false)
    .with_context(|| {
      format!("re-encrypting canonical secret {name:?} to master recipients")
    })
}

/// Rotate canonical secrets to their configured recipients, retaining host
/// access for secrets that are not rekeyed per host. When `name_filter` is set,
/// only that exact secret is rotated.
pub fn rekey_master(
  cfg: &Config,
  secrets_dir: &Path,
  name_filter: Option<&str>,
) -> Result<usize> {
  let identities = admin::master_identities(cfg)?;
  let recipients = identity::build_recipients(&cfg.master.recipients)?;

  if let Some(name) = name_filter {
    let secret = cfg
      .secrets
      .get(name)
      .with_context(|| format!("unknown secret {name:?}"))?;
    rekey_master_secret(
      cfg,
      secrets_dir,
      name,
      secret,
      &identities,
      &recipients,
    )?;
    return Ok(1);
  }

  for (name, secret) in &cfg.secrets {
    rekey_master_secret(
      cfg,
      secrets_dir,
      name,
      secret,
      &identities,
      &recipients,
    )?;
  }
  Ok(cfg.secrets.len())
}
