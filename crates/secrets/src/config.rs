//! Evaluated administration and runtime-manifest models.
//!
//! [`Config`] is emitted by the flake-wide Secrets inventory. [`Manifest`] is
//! emitted per host by the NixOS module and read by `secrets activate`.

use std::{
  collections::{BTreeMap, HashSet},
  path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The resolved admin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  pub version:    u32,
  pub storage:    PathBuf,
  pub master:     Master,
  pub hosts:      BTreeMap<String, Host>,
  pub secrets:    BTreeMap<String, Secret>,
  pub generators: BTreeMap<String, Generator>,
}

/// The identity that owns every canonical secret and performs edits/rekeys.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Master {
  /// Recipients that rekeyed *canonical* secrets are encrypted to. Typically
  /// a YubiKey recipient plus a backup age/SSH key.
  pub recipients: Vec<String>,
  /// Identity files the admin CLI decrypts canonical secrets with. `~` is
  /// expanded. A plugin identity (see [`Master::plugin`]) is tried too.
  #[serde(default)]
  pub identities: Vec<PathBuf>,
  /// Optional age plugin used as an admin identity, e.g. `"yubikey"`.
  #[serde(default)]
  pub plugin:     Option<String>,
}

/// A host and its decryption public keys.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
  pub pubkeys: Vec<String>,
}

/// A resolved secret with canonical and per-host ciphertext paths filled in
/// and target hosts expanded to concrete host names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
  pub file:       PathBuf,
  #[serde(rename = "hostFiles")]
  pub host_files: BTreeMap<String, PathBuf>,
  pub hosts:      Vec<String>,
  pub rekeyed:    bool,
  pub generator:  Option<String>,
}

/// A generator built for each supported Nix platform.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generator {
  #[serde(default)]
  pub args:     Vec<String>,
  pub packages: BTreeMap<String, GeneratorPackage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorPackage {
  pub derivation: String,
  pub executable: PathBuf,
}
type CiphertextPaths<'a> = BTreeMap<&'a Path, (&'a str, Option<&'a str>)>;

impl Config {
  /// Load and validate an evaluated JSON manifest.
  pub fn load(path: &Path) -> Result<Self> {
    let text = std::fs::read_to_string(path)
      .with_context(|| format!("reading admin manifest {}", path.display()))?;
    Self::parse(&text).with_context(|| format!("validating {}", path.display()))
  }

  pub fn parse(text: &str) -> Result<Self> {
    let config: Self =
      serde_json::from_str(text).context("parsing admin manifest")?;
    config.validate()?;
    Ok(config)
  }

  /// Recipients a rekeyed secret must be encrypted to for `host`.
  pub fn host_recipients(&self, host: &str) -> Result<&[String]> {
    self
      .hosts
      .get(host)
      .map(|h| h.pubkeys.as_slice())
      .with_context(|| format!("unknown host {host:?}"))
  }

  fn validate(&self) -> Result<()> {
    if self.version != 2 {
      bail!("unsupported admin manifest version {}", self.version);
    }
    if self.master.recipients.is_empty() {
      bail!("[master].recipients must list at least one recipient");
    }
    validate_relative_path(&self.storage).context("invalid storage path")?;
    let mut ciphertext_paths = BTreeMap::new();
    for (name, secret) in &self.secrets {
      self.validate_secret(name, secret, &mut ciphertext_paths)?;
    }
    Ok(())
  }

  fn validate_secret<'a>(
    &self,
    name: &'a str,
    secret: &'a Secret,
    ciphertext_paths: &mut CiphertextPaths<'a>,
  ) -> Result<()> {
    Self::validate_secret_file(name, secret, ciphertext_paths)?;
    if secret.hosts.is_empty() {
      bail!("secret {name:?} has no hosts");
    }
    for host in &secret.hosts {
      self.validate_secret_host(name, secret, host, ciphertext_paths)?;
    }
    for host in secret.host_files.keys() {
      if !secret.hosts.contains(host) {
        bail!(
          "secret {name:?} has a ciphertext path for non-target host {host:?}"
        );
      }
    }
    if let Some(gen_name) = &secret.generator
      && !self.generators.contains_key(gen_name)
    {
      bail!("secret {name:?} uses unknown generator {gen_name:?}");
    }
    Ok(())
  }

  fn validate_secret_file<'a>(
    name: &'a str,
    secret: &'a Secret,
    ciphertext_paths: &mut CiphertextPaths<'a>,
  ) -> Result<()> {
    validate_relative_path(Path::new(name))
      .with_context(|| format!("invalid secret name {name:?}"))?;
    validate_relative_path(&secret.file).with_context(|| {
      format!("invalid ciphertext path for secret {name:?}")
    })?;
    reserve_ciphertext_path(ciphertext_paths, &secret.file, name, None)?;
    Ok(())
  }

  fn validate_secret_host<'a>(
    &self,
    name: &'a str,
    secret: &'a Secret,
    host: &'a str,
    ciphertext_paths: &mut CiphertextPaths<'a>,
  ) -> Result<()> {
    let recipients = self.host_recipients(host)?;
    if recipients.is_empty() {
      bail!("host {host:?} has no recipients");
    }
    if secret.rekeyed {
      let host_file = secret.host_files.get(host).with_context(|| {
        format!("secret {name:?} has no ciphertext path for host {host:?}")
      })?;
      validate_relative_path(host_file).with_context(|| {
        format!("invalid ciphertext path for secret {name:?} on host {host:?}")
      })?;
      reserve_ciphertext_path(ciphertext_paths, host_file, name, Some(host))?;
    }
    Ok(())
  }
}
fn reserve_ciphertext_path<'a>(
  paths: &mut CiphertextPaths<'a>,
  path: &'a Path,
  name: &'a str,
  host: Option<&'a str>,
) -> Result<()> {
  if let Some((other_name, other_host)) = paths.insert(path, (name, host)) {
    bail!(
      "ciphertext path {} is shared by secret {other_name:?} host \
       {other_host:?} and secret {name:?} host {host:?}",
      path.display()
    );
  }
  Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
  if path.as_os_str().is_empty()
    || path
      .components()
      .any(|component| !matches!(component, Component::Normal(_)))
  {
    bail!("path must be a non-empty relative path without `.` or `..`");
  }
  Ok(())
}

/// Per-host runtime manifest, emitted by the NixOS module.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
  /// Ramfs store containing atomic generations (`/run/secrets`).
  pub store_dir:     PathBuf,
  /// Directory of stable links to non-isolated secrets (`/run/secrets.d`).
  pub secrets_dir:   PathBuf,
  /// Directory of stable links to non-isolated templates (`/run/secrets.t`).
  pub templates_dir: PathBuf,
  /// Optional stable pointer to the last published manifest across store
  /// changes.
  #[serde(default)]
  pub state_file:    Option<PathBuf>,
  /// Identity files used to decrypt on this host, in try order.
  #[serde(default)]
  pub identities:    Vec<PathBuf>,
  /// Optional age plugin identity name for host decryption (e.g. `yubikey`).
  #[serde(default)]
  pub plugin:        Option<String>,
  pub secrets:       Vec<ManifestSecret>,
  #[serde(default)]
  pub templates:     Vec<ManifestTemplate>,
}

/// One secret to install on a host.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSecret {
  pub name:        String,
  /// Absolute path to the ciphertext to decrypt (a Nix store path).
  pub file:        PathBuf,
  /// Where consumers expect the plaintext (`secrets_dir/<name>` for
  /// non-isolated secrets, or the in-namespace bind target for isolated).
  pub path:        PathBuf,
  pub owner:       String,
  pub group:       String,
  pub mode:        String,
  /// Opaque token replaced with this secret's plaintext in templates.
  pub placeholder: String,
  /// Isolated secrets are materialized only in `store_dir`; systemd binds
  /// them into their consumers' mount namespaces. Non-isolated secrets are
  /// additionally installed at `path` under `secrets_dir`.
  #[serde(default)]
  pub isolated:    bool,
}

/// One secret-bearing template to render on a host.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTemplate {
  pub name:     String,
  /// Absolute path to the placeholder-only template source in the Nix store.
  pub file:     PathBuf,
  /// Where consumers expect the rendered template.
  pub path:     PathBuf,
  pub owner:    String,
  pub group:    String,
  pub mode:     String,
  #[serde(default)]
  pub isolated: bool,
}

impl Manifest {
  pub fn load(path: &Path) -> Result<Self> {
    let text = std::fs::read_to_string(path)
      .with_context(|| format!("reading manifest {}", path.display()))?;
    let manifest: Self = serde_json::from_str(&text)
      .with_context(|| format!("parsing manifest {}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
  }

  fn validate(&self) -> Result<()> {
    if !self.store_dir.is_absolute()
      || !self.secrets_dir.is_absolute()
      || !self.templates_dir.is_absolute()
    {
      bail!("manifest runtime directories must be absolute");
    }
    if self
      .state_file
      .as_ref()
      .is_some_and(|path| !path.is_absolute())
    {
      bail!("manifest state file must be absolute");
    }
    if self
      .state_file
      .as_ref()
      .is_some_and(|path| path.starts_with(&self.store_dir))
    {
      bail!("manifest state file must be outside the generation store");
    }
    self.validate_outputs()
  }

  fn validate_outputs(&self) -> Result<()> {
    let mut placeholders = HashSet::new();
    let mut output_paths = HashSet::new();
    for secret in &self.secrets {
      Self::validate_secret(secret, &mut placeholders, &mut output_paths)?;
    }
    for template in &self.templates {
      Self::validate_template(template, &mut output_paths)?;
    }
    validate_staged_names(
      self
        .secrets
        .iter()
        .map(|secret| (secret.name.as_str(), secret.isolated)),
      "secret",
    )?;
    validate_staged_names(
      self
        .templates
        .iter()
        .map(|template| (template.name.as_str(), template.isolated)),
      "template",
    )?;
    validate_output_paths(&mut output_paths, self.state_file.as_deref())
  }
  fn validate_secret<'a>(
    secret: &'a ManifestSecret,
    placeholders: &mut HashSet<&'a String>,
    output_paths: &mut HashSet<&'a Path>,
  ) -> Result<()> {
    validate_relative_path(Path::new(&secret.name)).with_context(|| {
      format!("invalid manifest secret name {:?}", secret.name)
    })?;
    if !secret.file.is_absolute() || !secret.path.is_absolute() {
      bail!(
        "manifest paths for secret {:?} must be absolute",
        secret.name
      );
    }
    if secret.placeholder.is_empty()
      || !placeholders.insert(&secret.placeholder)
    {
      bail!(
        "manifest placeholder for secret {:?} is empty or duplicated",
        secret.name
      );
    }
    if !output_paths.insert(&secret.path) {
      bail!(
        "manifest output path {} is used more than once",
        secret.path.display()
      );
    }
    Ok(())
  }

  fn validate_template<'a>(
    template: &'a ManifestTemplate,
    output_paths: &mut HashSet<&'a Path>,
  ) -> Result<()> {
    validate_relative_path(Path::new(&template.name)).with_context(|| {
      format!("invalid manifest template name {:?}", template.name)
    })?;
    if !template.file.is_absolute() || !template.path.is_absolute() {
      bail!(
        "manifest paths for template {:?} must be absolute",
        template.name
      );
    }
    if !output_paths.insert(&template.path) {
      bail!(
        "manifest output path {} is used more than once",
        template.path.display()
      );
    }
    Ok(())
  }
}

fn validate_output_paths<'a>(
  output_paths: &mut HashSet<&'a Path>,
  state_file: Option<&'a Path>,
) -> Result<()> {
  if let Some(state_file) = state_file
    && !output_paths.insert(state_file)
  {
    bail!("manifest state file overlaps an output path");
  }
  for path in output_paths.iter() {
    for ancestor in path.ancestors().skip(1) {
      if output_paths.contains(ancestor) {
        bail!(
          "manifest output path {} contains another output",
          ancestor.display()
        );
      }
    }
  }
  Ok(())
}

fn validate_staged_names<'a>(
  entries: impl Iterator<Item = (&'a str, bool)>,
  kind: &str,
) -> Result<()> {
  let mut names = HashSet::new();
  for (name, isolated) in entries {
    if !names.insert((Path::new(name), isolated)) {
      bail!("duplicate manifest {kind} name {name:?}");
    }
  }
  for (name, isolated) in &names {
    for ancestor in name.ancestors().skip(1) {
      if names.contains(&(ancestor, *isolated)) {
        bail!(
          "manifest {kind} name {} contains another output",
          ancestor.display()
        );
      }
    }
  }
  Ok(())
}
