//! `secrets` command-line interface.

use std::{
  io::{Read, Write},
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use zeroize::Zeroizing;

use crate::config::Config;

/// age-compatible secrets manager.
#[derive(Parser)]
#[command(name = "secrets")]
struct Cli {
  /// Flake exporting `secrets.manifest`.
  #[arg(long, global = true, default_value = ".", env = "SECRETS_FLAKE")]
  flake: String,

  /// Evaluated admin or activation manifest, bypassing flake evaluation.
  #[arg(long, global = true, env = "SECRETS_MANIFEST")]
  manifest: Option<PathBuf>,

  /// Override the writable ciphertext storage directory.
  #[arg(long, global = true, env = "SECRETS_STORAGE")]
  storage: Option<PathBuf>,

  #[command(subcommand)]
  cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
  /// Encrypt input to one or more recipients (age, SSH, or plugin keys).
  Encrypt {
    /// Recipient public key (repeatable).
    #[arg(short, long = "recipient")]
    recipients:      Vec<String>,
    /// File of recipients, one per line (repeatable).
    #[arg(short = 'R', long)]
    recipients_file: Vec<PathBuf>,
    /// Write ciphertext here instead of stdout.
    #[arg(short, long)]
    output:          Option<PathBuf>,
    /// Emit ASCII-armored output.
    #[arg(short, long)]
    armor:           bool,
    /// Read plaintext from this file instead of stdin.
    input:           Option<PathBuf>,
  },
  /// Decrypt input with one or more identity files.
  Decrypt {
    /// Identity file (repeatable).
    #[arg(short, long = "identity")]
    identities: Vec<PathBuf>,
    /// age plugin name to use as an identity (e.g. yubikey).
    #[arg(short = 'j', long)]
    plugin:     Option<String>,
    /// Write plaintext here instead of stdout.
    #[arg(short, long)]
    output:     Option<PathBuf>,
    /// Read ciphertext from this file instead of stdin.
    input:      Option<PathBuf>,
  },
  /// Create or edit a secret in $EDITOR.
  Edit {
    /// Canonical secret name from the evaluated inventory.
    name: String,
  },
  /// Re-encrypt canonical secrets to their hosts.
  Rekey {
    /// Only rekey secrets targeting this host.
    #[arg(long)]
    host: Option<String>,
  },
  /// Rotate canonical secrets to their configured recipients.
  RekeyMaster {
    /// Only re-encrypt this canonical secret.
    #[arg(long)]
    secret: Option<String>,
  },
  /// Generate a secret from its configured generator.
  Generate {
    /// Canonical secret name from the evaluated inventory.
    name:  String,
    /// Overwrite an existing canonical secret.
    #[arg(long)]
    force: bool,
  },
  /// Check that every secret is present and rekeyed.
  Verify,
  /// Generate a fresh age identity.
  Keygen {
    /// Write the identity here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
  },
  /// Print resolved recipients for hosts or a secret.
  Recipients {
    /// Print recipients for this host.
    #[arg(long)]
    host:   Option<String>,
    /// Print recipients for this secret.
    #[arg(long)]
    secret: Option<String>,
  },
  /// Decrypt and install a host's secrets from a manifest (runs as root).
  Activate,
}

fn read_input(path: Option<&Path>) -> Result<Zeroizing<Vec<u8>>> {
  if let Some(p) = path {
    std::fs::read(p)
      .map(Zeroizing::new)
      .with_context(|| format!("reading {}", p.display()))
  } else {
    let mut buf = Vec::new();
    std::io::stdin()
      .read_to_end(&mut buf)
      .context("reading stdin")?;
    Ok(Zeroizing::new(buf))
  }
}

fn write_output(path: Option<&Path>, data: &[u8]) -> Result<()> {
  if let Some(p) = path {
    std::fs::write(p, data).with_context(|| format!("writing {}", p.display()))
  } else {
    std::io::stdout().write_all(data).context("writing stdout")
  }
}

fn load_admin(
  flake: &str,
  manifest: Option<&Path>,
  storage: Option<&Path>,
) -> Result<(Config, PathBuf)> {
  let cfg = if let Some(manifest) = manifest {
    Config::load(manifest)?
  } else {
    let output =
      Command::new(std::env::var_os("NIX").unwrap_or_else(|| "nix".into()))
        .args(["eval", "--raw"])
        .arg(format!("{flake}#secrets.manifest"))
        .output()
        .with_context(|| format!("evaluating Secrets inventory from {flake}"))?;
    if !output.status.success() {
      anyhow::bail!(
        "evaluating Secrets inventory failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    Config::parse(
      std::str::from_utf8(&output.stdout).context("manifest is not UTF-8")?,
    )?
  };

  let storage = storage_root(flake, manifest, storage, &cfg.storage)?;
  Ok((cfg, storage))
}

fn storage_root(
  flake: &str,
  manifest: Option<&Path>,
  storage: Option<&Path>,
  relative: &Path,
) -> Result<PathBuf> {
  if let Some(storage) = storage {
    Ok(storage.to_owned())
  } else if manifest.is_some() || flake == "." {
    Ok(
      std::env::current_dir()
        .context("resolving current directory")?
        .join(relative),
    )
  } else {
    let flake = Path::new(flake);
    if flake.is_dir() {
      Ok(flake.join(relative))
    } else {
      anyhow::bail!(
        "flake {:?} is not a writable local directory; pass --storage",
        flake
      )
    }
  }
}

fn encrypt_input(
  recipients: &[String],
  recipients_file: &[PathBuf],
  output: Option<&Path>,
  armor: bool,
  input: Option<&Path>,
) -> Result<()> {
  let mut recips = Vec::with_capacity(recipients.len());
  for recipient in recipients {
    recips.push(crate::parse_recipient(recipient)?);
  }
  for file in recipients_file {
    recips.extend(crate::read_recipients_file(file)?);
  }
  let plaintext = read_input(input)?;
  let ciphertext = crate::encrypt(&plaintext, &recips, armor)?;
  write_output(output, &ciphertext)
}

fn decrypt_input(
  identities: &[PathBuf],
  plugin: Option<&str>,
  output: Option<&Path>,
  input: Option<&Path>,
) -> Result<()> {
  let ids = crate::load_identities(identities, plugin)?;
  let ciphertext = read_input(input)?;
  let plaintext = crate::decrypt(&ciphertext, &ids)?;
  if let Some(path) = output {
    crate::secretio::write_atomic(path, &plaintext, 0o600)
      .with_context(|| format!("writing plaintext {}", path.display()))
  } else {
    std::io::stdout()
      .write_all(&plaintext)
      .context("writing stdout")
  }
}

/// Run Secrets with an argv slice whose first element is the invoked command.
pub fn run(args: &[String]) -> Result<()> {
  // SAFETY: PR_SET_DUMPABLE only changes a property of this process and the
  // constant argument layout is defined by Linux's prctl(2) ABI.
  if unsafe { nix::libc::prctl(nix::libc::PR_SET_DUMPABLE, 0) } != 0 {
    return Err(std::io::Error::last_os_error())
      .context("disabling process dumps");
  }

  let Cli {
    flake,
    manifest,
    storage,
    cmd,
  } = Cli::parse_from(args);

  match cmd {
    Cmd::Encrypt {
      recipients,
      recipients_file,
      output,
      armor,
      input,
    } => {
      encrypt_input(
        &recipients,
        &recipients_file,
        output.as_deref(),
        armor,
        input.as_deref(),
      )
    },
    Cmd::Decrypt {
      identities,
      plugin,
      output,
      input,
    } => {
      decrypt_input(
        &identities,
        plugin.as_deref(),
        output.as_deref(),
        input.as_deref(),
      )
    },
    Cmd::Keygen { output } => crate::tools::keygen(output.as_deref()),
    Cmd::Activate => {
      let manifest = manifest
        .as_deref()
        .context("activate requires --manifest")?;
      crate::activate::activate(manifest)
    },
    cmd => run_admin(cmd, &flake, manifest.as_deref(), storage.as_deref()),
  }
}

fn lock_admin_storage(storage: &Path) -> Result<std::fs::File> {
  // Create the storage root before locking so independently stored
  // inventories do not block one another during a long editor session.
  std::fs::create_dir_all(storage)
    .with_context(|| format!("creating storage {}", storage.display()))?;
  let lock = std::fs::File::open(storage).with_context(|| {
    format!("opening storage lock on {}", storage.display())
  })?;
  lock
    .lock()
    .with_context(|| format!("locking storage {}", storage.display()))?;
  Ok(lock)
}

fn run_admin(
  cmd: Cmd,
  flake: &str,
  manifest: Option<&Path>,
  storage: Option<&Path>,
) -> Result<()> {
  let (cfg, storage) = load_admin(flake, manifest, storage)?;
  let _lock = if matches!(
    cmd,
    Cmd::Edit { .. }
      | Cmd::Rekey { .. }
      | Cmd::RekeyMaster { .. }
      | Cmd::Generate { .. }
  ) {
    Some(lock_admin_storage(&storage)?)
  } else {
    None
  };
  execute_admin_command(cmd, &cfg, &storage)
}

fn execute_admin_command(cmd: Cmd, cfg: &Config, storage: &Path) -> Result<()> {
  match cmd {
    Cmd::Edit { name } => crate::editor::edit(cfg, storage, &name),
    Cmd::Rekey { host } => {
      let n = crate::rekey::rekey_all(cfg, storage, host.as_deref())?;
      eprintln!("rekeyed {n} artifact(s)");
      Ok(())
    },
    Cmd::RekeyMaster { secret } => {
      let n = crate::rekey::rekey_master(cfg, storage, secret.as_deref())?;
      eprintln!(
        "re-encrypted {n} canonical secret(s) to configured recipients"
      );
      Ok(())
    },
    Cmd::Generate { name, force } => {
      crate::generate::generate(cfg, storage, &name, force)
    },
    Cmd::Verify => crate::verify::verify(cfg, storage),
    Cmd::Recipients { host, secret } => {
      crate::tools::recipients(cfg, host.as_deref(), secret.as_deref())
    },
    _ => anyhow::bail!("expected a Secrets administrative command"),
  }
}
