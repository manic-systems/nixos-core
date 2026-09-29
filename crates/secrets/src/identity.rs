//! Recipient and identity parsing for age native keys, OpenSSH keys, and age
//! plugins (e.g. YubiKey).
//!
//! A *recipient* is a public key we encrypt to; an *identity* is a private key
//! we decrypt with. secrets accepts:
//!
//! * native age keys (`age1...` / `AGE-SECRET-KEY-...`),
//! * OpenSSH keys (`ssh-ed25519 ...` and matching private key files),
//! * age plugin recipients (`age1<plugin>1...`) and identities
//!   (`AGE-PLUGIN-<PLUGIN>-1...`), dispatched to the `age-plugin-<plugin>`
//!   binary.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
};

use age::{Identity, Recipient};
use anyhow::{Context, Result, anyhow, bail};

/// Expand a leading `~/` to the current user's home directory.
pub fn expand_tilde(path: &Path) -> PathBuf {
  if let Ok(rest) = path.strip_prefix("~")
    && let Some(home) = std::env::var_os("HOME")
  {
    return Path::new(&home).join(rest);
  }
  path.to_path_buf()
}

/// Parse a single recipient from a public-key string (age, SSH, or plugin).
pub fn parse_recipient(s: &str) -> Result<Box<dyn Recipient + Send>> {
  let s = s.trim();
  if s.is_empty() {
    bail!("empty recipient string");
  }

  if let Ok(r) = s.parse::<age::x25519::Recipient>() {
    return Ok(Box::new(r));
  }

  if s.starts_with("ssh-") {
    let r = s
      .parse::<age::ssh::Recipient>()
      .map_err(|e| anyhow!("invalid SSH recipient {s:?}: {e:?}"))?;
    return Ok(Box::new(r));
  }

  if s.starts_with("age1") {
    // Not a bare x25519 key (that branch is above), so treat it as a plugin
    // recipient such as `age1yubikey1...`.
    let plugin_recipient = s
      .parse::<age::plugin::Recipient>()
      .map_err(|e| anyhow!("invalid plugin recipient {s:?}: {e}"))?;
    let plugin_name = plugin_recipient.plugin().to_owned();
    let r = age::plugin::RecipientPluginV1::new(
      &plugin_name,
      &[plugin_recipient],
      &[],
      age::cli_common::UiCallbacks,
    )
    .map_err(|e| anyhow!("initializing age-plugin-{plugin_name}: {e}"))?;
    return Ok(Box::new(r));
  }

  bail!("unrecognized recipient (expected age1..., ssh-...): {s:?}")
}

/// Parse a list of recipient strings.
pub fn build_recipients<S: AsRef<str>>(
  specs: &[S],
) -> Result<Vec<Box<dyn Recipient + Send>>> {
  specs.iter().map(|s| parse_recipient(s.as_ref())).collect()
}

/// Read a recipients file (one recipient per line; blank lines and `#`
/// comments ignored), matching the `age -R` / `authorized_keys` convention.
pub fn read_recipients_file(
  path: &Path,
) -> Result<Vec<Box<dyn Recipient + Send>>> {
  let path = expand_tilde(path);
  let text = std::fs::read_to_string(&path)
    .with_context(|| format!("reading recipients file {}", path.display()))?;
  let mut out = Vec::new();
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    out.push(parse_recipient(line)?);
  }
  Ok(out)
}

/// Load decryption identities from a set of key files, plus an optional age
/// plugin used for any plugin identities encountered (and, if `plugin` is set
/// but no plugin identity file is supplied, the plugin's default identities).
pub fn load_identities(
  files: &[PathBuf],
  plugin: Option<&str>,
) -> Result<Vec<Box<dyn Identity>>> {
  let mut identities: Vec<Box<dyn Identity>> = Vec::new();
  // Plugin identities are grouped per plugin so each plugin binary is invoked
  // once with all of its identities.
  let mut plugin_ids: BTreeMap<String, Vec<age::plugin::Identity>> =
    BTreeMap::new();

  for file in files {
    load_identity_file(file, &mut identities, &mut plugin_ids)?;
  }

  // If a plugin was named but produced no explicit identity lines, ask the
  // plugin for its default identities (e.g. an inserted YubiKey).
  if let Some(name) = plugin
    && !plugin_ids.contains_key(name)
  {
    plugin_ids.entry(name.to_owned()).or_default();
  }

  for (name, ids) in plugin_ids {
    let plugin = age::plugin::IdentityPluginV1::new(
      &name,
      &ids,
      age::cli_common::UiCallbacks,
    )
    .map_err(|e| anyhow!("initializing age-plugin-{name}: {e}"))?;
    identities.push(Box::new(plugin));
  }

  Ok(identities)
}

fn load_identity_file(
  file: &Path,
  identities: &mut Vec<Box<dyn Identity>>,
  plugin_ids: &mut BTreeMap<String, Vec<age::plugin::Identity>>,
) -> Result<()> {
  let file = expand_tilde(file);
  if !file
    .try_exists()
    .with_context(|| format!("checking identity file {}", file.display()))?
  {
    eprintln!("secrets: skipping missing identity file {}", file.display());
    return Ok(());
  }
  let data = std::fs::read(&file)
    .with_context(|| format!("reading identity file {}", file.display()))?;
  load_identity_data(&file, &data, identities, plugin_ids)
}

fn load_identity_data(
  file: &Path,
  data: &[u8],
  identities: &mut Vec<Box<dyn Identity>>,
  plugin_ids: &mut BTreeMap<String, Vec<age::plugin::Identity>>,
) -> Result<()> {
  if let Ok(ssh) =
    age::ssh::Identity::from_buffer(data, Some(file.display().to_string()))
  {
    match ssh {
      age::ssh::Identity::Unsupported(k) => {
        bail!("unsupported SSH key in {}: {k:?}", file.display());
      },
      usable => {
        identities.push(Box::new(
          usable.with_callbacks(age::cli_common::UiCallbacks),
        ));
        return Ok(());
      },
    }
  }

  let text = String::from_utf8_lossy(data);
  if text.contains("AGE-PLUGIN-") {
    load_plugin_identities(file, &text, plugin_ids)?;
    return Ok(());
  }

  let parsed = age::IdentityFile::from_buffer(data).with_context(|| {
    format!("parsing {} as an age identity file", file.display())
  })?;
  identities.extend(
    parsed
      .into_identities()
      .with_context(|| format!("loading identities from {}", file.display()))?
      .into_iter()
      .map(|i| i as Box<dyn Identity>),
  );
  Ok(())
}

fn load_plugin_identities(
  file: &Path,
  text: &str,
  plugin_ids: &mut BTreeMap<String, Vec<age::plugin::Identity>>,
) -> Result<()> {
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let id = line.parse::<age::plugin::Identity>().map_err(|e| {
      anyhow!("invalid plugin identity in {}: {e}", file.display())
    })?;
    plugin_ids
      .entry(id.plugin().to_owned())
      .or_default()
      .push(id);
  }
  Ok(())
}
