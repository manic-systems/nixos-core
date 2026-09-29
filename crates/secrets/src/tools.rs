//! Small key utilities: generate a native age identity, and print resolved
//! recipients for hosts or secrets.

use std::path::Path;

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result};

use crate::{config::Config, secretio};

/// Generate a fresh age X25519 identity. The secret is written to `output`
/// (mode `0600`) or stdout; the public key is printed to stderr.
pub fn keygen(output: Option<&Path>) -> Result<()> {
  let identity = age::x25519::Identity::generate();
  let public = identity.to_public();
  let secret = identity.to_string();

  let body = format!(
    "# created by secrets\n# public key: {public}\n{}\n",
    secret.expose_secret()
  );

  match output {
    Some(path) => {
      secretio::write_atomic(path, body.as_bytes(), 0o600)?;
      eprintln!("Public key: {public}");
    },
    None => {
      print!("{body}");
      eprintln!("Public key: {public}");
    },
  }
  Ok(())
}

/// Print the recipients secrets would encrypt to for a host or a secret.
pub fn recipients(
  cfg: &Config,
  host: Option<&str>,
  secret: Option<&str>,
) -> Result<()> {
  if let Some(host) = host {
    for r in cfg.host_recipients(host)? {
      println!("{r}");
    }
    return Ok(());
  }
  if let Some(name) = secret {
    let secret = cfg
      .secrets
      .get(name)
      .with_context(|| format!("unknown secret {name:?}"))?;
    if secret.rekeyed {
      for host in &secret.hosts {
        for r in cfg.host_recipients(host)? {
          println!("{host}\t{r}");
        }
      }
    } else {
      for r in &cfg.master.recipients {
        println!("master\t{r}");
      }
      for host in &secret.hosts {
        for r in cfg.host_recipients(host)? {
          println!("{host}\t{r}");
        }
      }
    }
    return Ok(());
  }
  for (name, host) in &cfg.hosts {
    for key in &host.pubkeys {
      println!("{name}\t{key}");
    }
  }
  Ok(())
}
