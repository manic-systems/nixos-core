//! Consistency checks over an evaluated manifest and its on-disk secret tree.

use std::path::Path;

use anyhow::{Result, bail};

use crate::{
  admin,
  config::{Config, Secret},
};

fn verify_secret(
  name: &str,
  secret: &Secret,
  secrets_dir: &Path,
  problems: &mut Vec<String>,
) -> Result<()> {
  let canonical = admin::canonical_path(secrets_dir, secret);
  if !canonical.exists() {
    problems.push(format!(
      "{name}: canonical file missing at {}",
      canonical.display()
    ));
    return Ok(());
  }
  if secret.hosts.is_empty() {
    problems.push(format!("{name}: no target hosts"));
  }
  if !secret.rekeyed {
    return Ok(());
  }
  let canonical_mtime = std::fs::metadata(&canonical)
    .and_then(|m| m.modified())
    .ok();
  for host in &secret.hosts {
    let rekeyed = admin::rekeyed_path(secrets_dir, host, secret)?;
    match std::fs::metadata(&rekeyed) {
      Err(_) => {
        problems
          .push(format!("{name}: not rekeyed for {host} (run `secrets rekey`)"))
      },
      Ok(meta) => {
        if let (Some(canon), Ok(rk)) = (canonical_mtime, meta.modified())
          && rk < canon
        {
          problems.push(format!(
            "{name}: rekeyed copy for {host} is older than canonical (run \
             `secrets rekey`)"
          ));
        }
      },
    }
  }
  Ok(())
}

/// Verify that every declared secret has its canonical file, and that every
/// rekeyed secret has an up-to-date artifact for each of its hosts. Returns an
/// error listing all problems if any are found.
pub fn verify(cfg: &Config, secrets_dir: &Path) -> Result<()> {
  let mut problems = Vec::new();
  for (name, secret) in &cfg.secrets {
    verify_secret(name, secret, secrets_dir, &mut problems)?;
  }

  if problems.is_empty() {
    println!("ok: {} secret(s) verified", cfg.secrets.len());
    Ok(())
  } else {
    for p in &problems {
      eprintln!("FAIL {p}");
    }
    bail!("{} problem(s) found", problems.len())
  }
}
