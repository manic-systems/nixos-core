//! Secret generation: run a declared Nix-built executable and store its stdout
//! as a freshly-encrypted canonical secret, then rekey it to its hosts.

use std::{path::Path, process::Command};

use anyhow::{Context, Result};
use zeroize::Zeroizing;

use crate::{admin, config::Config, rekey, secretio};

/// Generate (or regenerate) the secret `name` using its configured generator.
pub fn generate(
  cfg: &Config,
  secrets_dir: &Path,
  name: &str,
  force: bool,
) -> Result<()> {
  let secret = cfg
    .secrets
    .get(name)
    .with_context(|| format!("unknown secret {name:?}"))?;
  let gen_name = secret
    .generator
    .as_deref()
    .with_context(|| format!("secret {name:?} has no generator"))?;
  let generator = cfg
    .generators
    .get(gen_name)
    .with_context(|| format!("unknown generator {gen_name:?}"))?;
  let system = match (std::env::consts::ARCH, std::env::consts::OS) {
    ("x86_64", "linux") => "x86_64-linux",
    ("aarch64", "linux") => "aarch64-linux",
    (arch, os) => anyhow::bail!("unsupported generator platform {arch}-{os}"),
  };
  let package = generator.packages.get(system).with_context(|| {
    format!("generator {gen_name:?} has no package for {system}")
  })?;

  let canonical = admin::canonical_path(secrets_dir, secret);
  if canonical.exists() && !force {
    anyhow::bail!(
      "canonical secret {name:?} already exists at {}; pass --force to \
       regenerate",
      canonical.display()
    );
  }

  let build =
    Command::new(std::env::var_os("NIX").unwrap_or_else(|| "nix".into()))
      .args(["build", "--no-link"])
      .arg(format!("{}^out", package.derivation))
      .output()
      .with_context(|| format!("building generator {gen_name:?}"))?;
  if !build.status.success() {
    anyhow::bail!(
      "building generator {gen_name:?} failed ({}): {}",
      build.status,
      String::from_utf8_lossy(&build.stderr).trim()
    );
  }

  let output = Command::new(&package.executable)
    .args(&generator.args)
    .output()
    .with_context(|| format!("running generator {gen_name:?}"))?;
  if !output.status.success() {
    anyhow::bail!(
      "generator {gen_name:?} failed ({}): {}",
      output.status,
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  if output.stdout.is_empty() {
    anyhow::bail!("generator {gen_name:?} produced no output");
  }
  let plaintext = Zeroizing::new(output.stdout);

  let recipients = admin::canonical_recipients(cfg, secret)?;
  secretio::encrypt_to_file(&canonical, &plaintext, &recipients, false)
    .with_context(|| format!("writing canonical secret {name:?}"))?;

  let identities = admin::master_identities(cfg)?;
  let n =
    rekey::rekey_secret(cfg, secrets_dir, name, secret, &identities, None)?;
  eprintln!("generated {name} ({n} host(s) rekeyed)");
  Ok(())
}
