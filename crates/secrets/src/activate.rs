//! Transactional runtime activation.
//!
//! Every secret is decrypted and every template is rendered before the live
//! generation changes. Outputs are then written into a fresh ramfs generation
//! and published by atomically replacing `store_dir/current`. Isolated outputs
//! remain below a root-only subtree and are bind-mounted into their systemd
//! consumers; non-isolated outputs are exposed through stable symlinks.

use std::{
  collections::HashSet,
  fs,
  os::unix::fs::{PermissionsExt, symlink},
  path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use zeroize::Zeroizing;

use crate::{
  config::{Manifest, ManifestSecret, ManifestTemplate},
  identity,
  secretio,
};

#[derive(Clone, Copy)]
struct Metadata {
  mode: u32,
  uid:  u32,
  gid:  u32,
}

fn parse_mode(mode: &str) -> Result<u32> {
  let mode = u32::from_str_radix(mode.trim_start_matches("0o"), 8)
    .with_context(|| format!("invalid octal mode {mode:?}"))?;
  if mode > 0o7777 {
    bail!("mode {mode:o} contains bits outside the Unix permission mask");
  }
  Ok(mode)
}

fn resolve_metadata(owner: &str, group: &str, mode: &str) -> Result<Metadata> {
  // Early activation precedes /etc/passwd and /etc/group. Root is the only
  // owner allowed for that path, so resolve it without NSS.
  let uid = if owner == "root" || owner == "0" {
    0
  } else {
    nix::unistd::User::from_name(owner)
      .with_context(|| format!("looking up user {owner:?}"))?
      .with_context(|| format!("no such user {owner:?}"))?
      .uid
      .as_raw()
  };
  let gid = if group == "root" || group == "0" {
    0
  } else {
    nix::unistd::Group::from_name(group)
      .with_context(|| format!("looking up group {group:?}"))?
      .with_context(|| format!("no such group {group:?}"))?
      .gid
      .as_raw()
  };
  Ok(Metadata {
    mode: parse_mode(mode)?,
    uid,
    gid,
  })
}

fn ensure_dir(path: &Path, mode: u32) -> Result<()> {
  fs::create_dir_all(path)
    .with_context(|| format!("creating {}", path.display()))?;
  fs::set_permissions(path, fs::Permissions::from_mode(mode))
    .with_context(|| format!("chmod {} {:o}", path.display(), mode))?;
  Ok(())
}

/// Create every directory between `base` and the parent of `name` with an
/// explicit mode. The activator's restrictive umask must not make nested
/// public output names untraversable.
fn ensure_output_parents(base: &Path, name: &str, mode: u32) -> Result<()> {
  let Some(parent) = Path::new(name).parent() else {
    return Ok(());
  };
  let mut current = base.to_path_buf();
  for component in parent.components() {
    current.push(component);
    ensure_dir(&current, mode)?;
  }
  Ok(())
}

/// Whether `path` is a mount point, per `/proc/self/mountinfo` (field 5).
fn is_mountpoint(path: &Path) -> Result<bool> {
  let target = path.to_string_lossy();
  let info = fs::read_to_string("/proc/self/mountinfo")
    .context("reading mount table")?;
  Ok(
    info
      .lines()
      .filter_map(|line| line.split(' ').nth(4))
      .any(|mountpoint| mountpoint == target),
  )
}

fn require_ramfs(path: &Path) -> Result<()> {
  // RAMFS_MAGIC from linux/magic.h. An existing tmpfs or disk mount must not
  // receive plaintext, even if it already occupies the configured mountpoint.
  const RAMFS: nix::sys::statfs::FsType = nix::sys::statfs::FsType(0x8584_58F6);
  if nix::sys::statfs::statfs(path)
    .with_context(|| format!("checking filesystem at {}", path.display()))?
    .filesystem_type()
    != RAMFS
  {
    bail!("Secrets store {} is not ramfs", path.display());
  }
  Ok(())
}

/// Ensure the generation store is a non-swappable ramfs. The mount itself is
/// searchable so stable public links can reach non-isolated outputs; directory
/// listing is root-only, and the `private` subtree remains mode `0700`.
fn ensure_ramfs(path: &Path) -> Result<()> {
  fs::create_dir_all(path)
    .with_context(|| format!("creating {}", path.display()))?;
  if !is_mountpoint(path)? {
    nix::mount::mount(
      Some("ramfs"),
      path,
      Some("ramfs"),
      nix::mount::MsFlags::MS_NOSUID
        | nix::mount::MsFlags::MS_NODEV
        | nix::mount::MsFlags::MS_NOEXEC,
      Some("mode=0711"),
    )
    .with_context(|| format!("mounting ramfs at {}", path.display()))?;
  }
  require_ramfs(path)?;
  // Mounting resets the directory mode, and an existing mount may have been
  // created by an older Secrets release with mode 0700.
  fs::set_permissions(path, fs::Permissions::from_mode(0o711))
    .with_context(|| format!("chmod {}", path.display()))?;
  Ok(())
}

fn install(target: &Path, plaintext: &[u8], metadata: Metadata) -> Result<()> {
  secretio::write_atomic_with_owner(
    target,
    plaintext,
    metadata.mode,
    Some((metadata.uid, metadata.gid)),
  )
  .with_context(|| format!("installing {}", target.display()))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  (!needle.is_empty())
    .then(|| {
      haystack
        .windows(needle.len())
        .position(|window| window == needle)
    })
    .flatten()
}

/// Render a template in one pass so bytes inserted from one secret are never
/// interpreted as another placeholder. Unknown or malformed Secrets placeholders
/// fail closed instead of leaving a credential-looking token in the result.
fn render_template(
  source: &[u8],
  secrets: &[(&ManifestSecret, &Zeroizing<Vec<u8>>)],
) -> Result<Zeroizing<Vec<u8>>> {
  let marker = b"{{SECRETS-";
  let closing = b"}}";
  let mut scan = 0;
  while let Some(relative_start) = find_bytes(&source[scan..], marker) {
    let start = scan + relative_start;
    let tail = &source[start..];
    let Some(relative_end) = find_bytes(tail, closing) else {
      bail!("template contains an unterminated Secrets placeholder");
    };
    let end = start + relative_end + closing.len();
    let token = &source[start..end];
    if !secrets
      .iter()
      .any(|(secret, _)| secret.placeholder.as_bytes() == token)
    {
      bail!(
        "template references unknown Secrets placeholder {}",
        String::from_utf8_lossy(token)
      );
    }
    scan = end;
  }

  let mut output = Vec::with_capacity(source.len());
  let mut cursor = 0;
  while cursor < source.len() {
    let next = secrets
      .iter()
      .filter_map(|(secret, plaintext)| {
        find_bytes(&source[cursor..], secret.placeholder.as_bytes())
          .map(|offset| (offset, secret.placeholder.len(), &plaintext[..]))
      })
      .min_by_key(|(offset, ..)| *offset);
    let Some((offset, token_len, plaintext)) = next else {
      output.extend_from_slice(&source[cursor..]);
      break;
    };
    let start = cursor + offset;
    output.extend_from_slice(&source[cursor..start]);
    output.extend_from_slice(plaintext);
    cursor = start + token_len;
  }
  Ok(Zeroizing::new(output))
}

/// Atomically install or replace a symlink without ever truncating the old
/// output in place.
fn atomic_symlink(target: &Path, link: &Path) -> Result<()> {
  let parent = link
    .parent()
    .with_context(|| format!("symlink {} has no parent", link.display()))?;
  fs::create_dir_all(parent)
    .with_context(|| format!("creating {}", parent.display()))?;

  let placeholder = tempfile::Builder::new()
    .prefix(".secrets-link-")
    .tempfile_in(parent)
    .with_context(|| {
      format!("allocating temporary link in {}", parent.display())
    })?;
  let temporary = placeholder.path().to_path_buf();
  placeholder
    .close()
    .context("removing temporary link placeholder")?;
  symlink(target, &temporary).with_context(|| {
    format!("creating temporary symlink {}", temporary.display())
  })?;
  if let Err(error) = fs::rename(&temporary, link) {
    let _ = fs::remove_file(&temporary);
    return Err(error)
      .with_context(|| format!("publishing symlink {}", link.display()));
  }
  Ok(())
}

fn generation_target(
  store_dir: &Path,
  kind: &str,
  name: &str,
  isolated: bool,
) -> PathBuf {
  store_dir
    .join("current")
    .join(if isolated { "private" } else { "public" })
    .join(kind)
    .join(name)
}

fn prune_stale_link(path: &Path, managed_prefix: &Path) -> Result<bool> {
  let target = fs::read_link(path)
    .with_context(|| format!("reading symlink {}", path.display()))?;
  if target.starts_with(managed_prefix) {
    fs::remove_file(path)
      .with_context(|| format!("removing stale link {}", path.display()))?;
    return Ok(true);
  }
  Ok(false)
}

fn prune_link_entry(
  path: &Path,
  file_type: fs::FileType,
  keep: &HashSet<PathBuf>,
  managed_prefix: &Path,
) -> Result<bool> {
  if file_type.is_dir() {
    let removed_child = prune_links(path, keep, managed_prefix)?;
    if removed_child && fs::read_dir(path)?.next().is_none() {
      fs::remove_dir(path).with_context(|| {
        format!("removing empty directory {}", path.display())
      })?;
    }
    return Ok(removed_child);
  }
  if file_type.is_symlink() && !keep.contains(path) {
    return prune_stale_link(path, managed_prefix);
  }
  Ok(false)
}

fn prune_links(
  dir: &Path,
  keep: &HashSet<PathBuf>,
  managed_prefix: &Path,
) -> Result<bool> {
  let Ok(entries) = fs::read_dir(dir) else {
    return Ok(false);
  };
  let mut removed_managed_output = false;
  for entry in entries {
    let entry = entry?;
    let path = entry.path();
    removed_managed_output |=
      prune_link_entry(&path, entry.file_type()?, keep, managed_prefix)?;
  }
  Ok(removed_managed_output)
}

fn prune_generations(
  generations: &Path,
  keep: &HashSet<PathBuf>,
) -> Result<()> {
  for entry in fs::read_dir(generations).with_context(|| {
    format!("reading generations in {}", generations.display())
  })? {
    let entry = entry?;
    let path = entry.path();
    if entry.file_type()?.is_dir() && !keep.contains(&path) {
      fs::remove_dir_all(&path).with_context(|| {
        format!("removing stale generation {}", path.display())
      })?;
    }
  }
  Ok(())
}

fn prune_old_generations(
  generations: &Path,
  generation: PathBuf,
  previous: Option<PathBuf>,
) {
  // Preserve the previous generation for existing binds and open descriptors.
  let mut keep = HashSet::from([generation]);
  if let Some(previous) = previous
    && previous.starts_with(generations)
  {
    keep.insert(previous);
  }
  if let Err(error) = prune_generations(generations, &keep) {
    eprintln!("secrets: warning: could not prune stale generations: {error:#}");
  }
}

fn stage_secret(
  root: &Path,
  secret: &ManifestSecret,
  plaintext: &[u8],
  metadata: Metadata,
) -> Result<()> {
  let visibility = if secret.isolated { "private" } else { "public" };
  let base = root.join(visibility).join("secrets");
  ensure_output_parents(
    &base,
    &secret.name,
    if secret.isolated { 0o700 } else { 0o711 },
  )?;
  install(&base.join(&secret.name), plaintext, metadata)
}

fn stage_template(
  root: &Path,
  template: &ManifestTemplate,
  content: &[u8],
  metadata: Metadata,
) -> Result<()> {
  let visibility = if template.isolated {
    "private"
  } else {
    "public"
  };
  let base = root.join(visibility).join("templates");
  ensure_output_parents(
    &base,
    &template.name,
    if template.isolated { 0o700 } else { 0o711 },
  )?;
  install(&base.join(&template.name), content, metadata)
}

struct PreparedOutputs {
  plaintexts:        Vec<Zeroizing<Vec<u8>>>,
  rendered:          Vec<Zeroizing<Vec<u8>>>,
  secret_metadata:   Vec<Metadata>,
  template_metadata: Vec<Metadata>,
}

/// Resolve identities and decrypt all secrets before reading templates.
fn decrypt_secrets(manifest: &Manifest) -> Result<Vec<Zeroizing<Vec<u8>>>> {
  let identities = if manifest.secrets.is_empty() {
    Vec::new()
  } else {
    let identities = identity::load_identities(
      &manifest.identities,
      manifest.plugin.as_deref(),
    )
    .context("loading host identities")?;
    if identities.is_empty() {
      bail!(
        "no usable host identities; configure \
         system.nixos-core.secrets.identityPaths with an existing private key or \
         system.nixos-core.secrets.plugin"
      );
    }
    identities
  };

  let mut plaintexts = Vec::with_capacity(manifest.secrets.len());
  for secret in &manifest.secrets {
    plaintexts.push(
      secretio::decrypt_file(&secret.file, &identities)
        .with_context(|| format!("decrypting secret {:?}", secret.name))?,
    );
  }
  Ok(plaintexts)
}

fn render_templates(
  manifest: &Manifest,
  plaintexts: &[Zeroizing<Vec<u8>>],
) -> Result<Vec<Zeroizing<Vec<u8>>>> {
  let secret_values =
    manifest.secrets.iter().zip(plaintexts).collect::<Vec<_>>();
  let mut rendered = Vec::with_capacity(manifest.templates.len());
  for template in &manifest.templates {
    let source = fs::read(&template.file).with_context(|| {
      format!("reading template source {}", template.file.display())
    })?;
    rendered.push(
      render_template(&source, &secret_values)
        .with_context(|| format!("rendering template {:?}", template.name))?,
    );
  }
  Ok(rendered)
}

/// Resolve all fallible inputs before modifying the live generation.
fn prepare_outputs(manifest: &Manifest) -> Result<PreparedOutputs> {
  let plaintexts = decrypt_secrets(manifest)?;
  let rendered = render_templates(manifest, &plaintexts)?;
  let secret_metadata = manifest
    .secrets
    .iter()
    .map(|secret| resolve_metadata(&secret.owner, &secret.group, &secret.mode))
    .collect::<Result<Vec<_>>>()?;
  let template_metadata = manifest
    .templates
    .iter()
    .map(|template| {
      resolve_metadata(&template.owner, &template.group, &template.mode)
    })
    .collect::<Result<Vec<_>>>()?;

  Ok(PreparedOutputs {
    plaintexts,
    rendered,
    secret_metadata,
    template_metadata,
  })
}

/// Mount the generation store and make the stable output directories
/// searchable.
fn prepare_store(manifest: &Manifest) -> Result<PathBuf> {
  ensure_ramfs(&manifest.store_dir)?;
  ensure_dir(&manifest.secrets_dir, 0o755)?;
  ensure_dir(&manifest.templates_dir, 0o755)?;

  let generations = manifest.store_dir.join("generations");
  ensure_dir(&generations, 0o711)?;
  Ok(generations)
}

fn create_generation(generations: &Path) -> Result<tempfile::TempDir> {
  let generation = tempfile::Builder::new()
    .prefix("generation-")
    .tempdir_in(generations)
    .with_context(|| {
      format!("creating generation in {}", generations.display())
    })?;
  ensure_dir(generation.path(), 0o711)?;
  for path in [
    generation.path().join("private"),
    generation.path().join("private/secrets"),
    generation.path().join("private/templates"),
  ] {
    ensure_dir(&path, 0o700)?;
  }
  for path in [
    generation.path().join("public"),
    generation.path().join("public/secrets"),
    generation.path().join("public/templates"),
  ] {
    ensure_dir(&path, 0o711)?;
  }
  Ok(generation)
}

fn stage_outputs(
  manifest: &Manifest,
  prepared: &PreparedOutputs,
  generation: &Path,
) -> Result<()> {
  for ((secret, metadata), plaintext) in manifest
    .secrets
    .iter()
    .zip(&prepared.secret_metadata)
    .zip(&prepared.plaintexts)
  {
    stage_secret(generation, secret, plaintext, *metadata)?;
  }
  for ((template, metadata), content) in manifest
    .templates
    .iter()
    .zip(&prepared.template_metadata)
    .zip(&prepared.rendered)
  {
    stage_template(generation, template, content, *metadata)?;
  }
  Ok(())
}

/// Install prepared outputs into a fresh ramfs generation, not the live view.
fn stage_generation(
  manifest: &Manifest,
  prepared: &PreparedOutputs,
) -> Result<(PathBuf, PathBuf)> {
  let generations = prepare_store(manifest)?;
  let generation = create_generation(&generations)?;
  let record = serde_json::to_vec(manifest).context("serializing manifest")?;
  secretio::write_atomic(
    &generation.path().join("manifest.json"),
    &record,
    0o600,
  )?;
  stage_outputs(manifest, prepared, generation.path())?;
  Ok((generation.keep(), generations))
}

/// Create stable links to `current` before switching the generation.
fn publish_output_links(
  manifest: &Manifest,
) -> Result<(HashSet<PathBuf>, HashSet<PathBuf>)> {
  let mut keep_secret_links = HashSet::new();
  for secret in &manifest.secrets {
    if !secret.isolated {
      ensure_output_parents(&manifest.secrets_dir, &secret.name, 0o711)?;
      atomic_symlink(
        &generation_target(&manifest.store_dir, "secrets", &secret.name, false),
        &secret.path,
      )?;
      keep_secret_links.insert(secret.path.clone());
    }
  }
  let mut keep_template_links = HashSet::new();
  for template in &manifest.templates {
    if !template.isolated {
      ensure_output_parents(&manifest.templates_dir, &template.name, 0o711)?;
      atomic_symlink(
        &generation_target(
          &manifest.store_dir,
          "templates",
          &template.name,
          false,
        ),
        &template.path,
      )?;
      keep_template_links.insert(template.path.clone());
    }
  }
  Ok((keep_secret_links, keep_template_links))
}

fn previous_manifest(path: &Path) -> Result<Option<Manifest>> {
  match fs::read(path) {
    Ok(data) => {
      let previous: Manifest = serde_json::from_slice(&data)
        .with_context(|| format!("parsing {}", path.display()))?;
      Ok(Some(previous))
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => {
      Err(error).with_context(|| format!("reading {}", path.display()))
    },
  }
}

fn prune_recorded_link(path: &Path, expected: &Path) -> Result<()> {
  if fs::read_link(path).is_ok_and(|target| target == expected) {
    fs::remove_file(path)
      .with_context(|| format!("removing stale link {}", path.display()))?;
  }
  Ok(())
}

fn prune_previous_outputs(previous: &Manifest, next: &Manifest) -> Result<()> {
  let keep: HashSet<&Path> = next
    .secrets
    .iter()
    .filter(|secret| !secret.isolated)
    .map(|secret| secret.path.as_path())
    .chain(
      next
        .templates
        .iter()
        .filter(|template| !template.isolated)
        .map(|template| template.path.as_path()),
    )
    .collect();
  for secret in &previous.secrets {
    if !secret.isolated && !keep.contains(secret.path.as_path()) {
      prune_recorded_link(
        &secret.path,
        &generation_target(&previous.store_dir, "secrets", &secret.name, false),
      )?;
    }
  }
  for template in &previous.templates {
    if !template.isolated && !keep.contains(template.path.as_path()) {
      prune_recorded_link(
        &template.path,
        &generation_target(
          &previous.store_dir,
          "templates",
          &template.name,
          false,
        ),
      )?;
    }
  }
  Ok(())
}

fn prune_published_links(
  manifest: &Manifest,
  previous: Option<&Manifest>,
  keep_secrets: &HashSet<PathBuf>,
  keep_templates: &HashSet<PathBuf>,
) {
  let managed_prefix = manifest.store_dir.join("current");
  if let Err(error) =
    prune_links(&manifest.secrets_dir, keep_secrets, &managed_prefix)
  {
    eprintln!("secrets: warning: could not prune stale secret links: {error:#}");
  }
  if let Err(error) =
    prune_links(&manifest.templates_dir, keep_templates, &managed_prefix)
  {
    eprintln!("secrets: warning: could not prune stale template links: {error:#}");
  }
  if let Some(previous) = previous
    && let Err(error) = prune_previous_outputs(previous, manifest)
  {
    eprintln!("secrets: warning: could not prune old output links: {error:#}");
  }
}

fn retire_previous_store(
  previous: &Manifest,
  next: &Manifest,
  old_manifest_target: &Path,
) -> Result<()> {
  if previous.store_dir == next.store_dir {
    return Ok(());
  }
  let current = previous.store_dir.join("current");
  if fs::read_link(&current)
    .is_ok_and(|target| old_manifest_target.parent() == Some(target.as_path()))
  {
    fs::remove_file(&current)
      .with_context(|| format!("retiring old store {}", current.display()))?;
  }
  Ok(())
}

/// Prepare stable links, atomically switch `current`, then prune stale outputs.
fn publish_generation(
  manifest: &Manifest,
  generation: PathBuf,
  generations: &Path,
) -> Result<()> {
  let current = manifest.store_dir.join("current");
  let previous = fs::read_link(&current).ok();
  let previous_manifest = if let Some(state_file) = &manifest.state_file {
    previous_manifest(state_file)?
  } else {
    previous_manifest(&current.join("manifest.json"))?
  };
  let old_manifest_target = manifest
    .state_file
    .as_ref()
    .and_then(|state_file| fs::read_link(state_file).ok());

  let (keep_secret_links, keep_template_links) =
    publish_output_links(manifest)?;

  // The single publication point: until this rename succeeds, every stable
  // link and every future namespace bind still resolves to the old content.
  atomic_symlink(&generation, &current)?;
  if let Some(state_file) = &manifest.state_file {
    atomic_symlink(&generation.join("manifest.json"), state_file)?;
  }

  prune_published_links(
    manifest,
    previous_manifest.as_ref(),
    &keep_secret_links,
    &keep_template_links,
  );
  if let (Some(previous), Some(old_target)) =
    (previous_manifest.as_ref(), old_manifest_target.as_deref())
  {
    retire_previous_store(previous, manifest, old_target)?;
  }

  prune_old_generations(generations, generation, previous);

  Ok(())
}

/// Decrypt and publish every output named in `manifest` as one generation.
pub fn activate(manifest_path: &Path) -> Result<()> {
  let manifest = Manifest::load(manifest_path)?;
  // Lock the stable pointer's parent across store relocations. Without one,
  // lock the store parent because mounting ramfs changes the store inode.
  let lock_root = manifest.state_file.as_ref().unwrap_or(&manifest.store_dir);
  let parent = lock_root
    .parent()
    .context("Secrets activation lock has no parent directory")?;
  fs::create_dir_all(parent)
    .with_context(|| format!("creating {}", parent.display()))?;
  let lock = fs::File::open(parent).with_context(|| {
    format!("opening activation lock on {}", parent.display())
  })?;
  lock.lock().with_context(|| {
    format!("locking activation parent {}", parent.display())
  })?;
  let prepared = prepare_outputs(&manifest)?;
  let (generation, generations) = stage_generation(&manifest, &prepared)?;
  publish_generation(&manifest, generation, &generations)?;

  eprintln!(
    "secrets: activated {} secret(s) and {} template(s)",
    manifest.secrets.len(),
    manifest.templates.len()
  );
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
  };

  use zeroize::Zeroizing;

  use super::{
    ensure_output_parents,
    generation_target,
    prune_previous_outputs,
    render_template,
    require_ramfs,
    retire_previous_store,
  };
  use crate::config::{Manifest, ManifestSecret};

  fn secret(name: &str, placeholder: &str) -> ManifestSecret {
    ManifestSecret {
      name:        name.to_owned(),
      file:        PathBuf::from("/ciphertext"),
      path:        PathBuf::from("/output"),
      owner:       "root".to_owned(),
      group:       "root".to_owned(),
      mode:        "400".to_owned(),
      placeholder: placeholder.to_owned(),
      isolated:    false,
    }
  }

  #[test]
  fn renders_multiple_and_repeated_secrets() {
    let first = secret("first", "{{SECRETS-first}}");
    let second = secret("second", "{{SECRETS-second}}");
    let one = Zeroizing::new(b"alpha".to_vec());
    let two = Zeroizing::new(b"beta".to_vec());
    let rendered = render_template(
      b"a={{SECRETS-first}} b={{SECRETS-second}} a2={{SECRETS-first}}",
      &[(&first, &one), (&second, &two)],
    )
    .expect("render template");
    assert_eq!(&*rendered, b"a=alpha b=beta a2=alpha");
  }

  #[test]
  fn does_not_expand_placeholder_bytes_inside_a_secret() {
    let first = secret("first", "{{SECRETS-first}}");
    let second = secret("second", "{{SECRETS-second}}");
    let one = Zeroizing::new(b"{{SECRETS-second}}".to_vec());
    let two = Zeroizing::new(b"must-not-appear".to_vec());
    let rendered = render_template(b"value={{SECRETS-first}}", &[
      (&first, &one),
      (&second, &two),
    ])
    .expect("render template");
    assert_eq!(&*rendered, b"value={{SECRETS-second}}");
  }

  #[test]
  fn rejects_unknown_placeholders() {
    let err = render_template(b"value={{SECRETS-unknown}}", &[])
      .expect_err("unknown placeholder must fail");
    assert!(format!("{err:#}").contains("unknown Secrets placeholder"));
  }

  #[test]
  fn refuses_a_non_ramfs_store_before_changing_its_permissions() {
    let error =
      require_ramfs(Path::new("/proc")).expect_err("procfs is not ramfs");
    assert!(format!("{error:#}").contains("not ramfs"));
  }

  #[test]
  fn nested_public_output_parents_are_searchable() {
    let dir = tempfile::tempdir().expect("temp directory");
    let base = dir.path().join("secrets.d");
    ensure_output_parents(&base, "service/matrix", 0o711)
      .expect("create public output parents");

    let mode = fs::metadata(base.join("service"))
      .expect("nested output parent metadata")
      .permissions()
      .mode()
      & 0o7777;
    assert_eq!(mode, 0o711);
  }
  #[test]
  fn prunes_only_recorded_obsolete_links_across_store_changes() {
    let dir = tempfile::tempdir().expect("temp directory");
    let old_store = dir.path().join("old-store");
    let old_path = dir.path().join("custom/old");
    let foreign_path = dir.path().join("custom/foreign");
    let old_generation = old_store.join("generations/old");
    fs::create_dir_all(&old_generation).expect("old generation");
    symlink(&old_generation, old_store.join("current")).expect("old current");
    fs::create_dir_all(old_path.parent().unwrap()).expect("custom parent");
    symlink(
      generation_target(&old_store, "secrets", "token", false),
      &old_path,
    )
    .expect("old managed link");
    symlink("/unrelated", &foreign_path).expect("foreign link");

    let mut token = secret("token", "{{SECRETS-token}}");
    token.path = old_path.clone();
    let mut foreign = secret("foreign", "{{SECRETS-foreign}}");
    foreign.path = foreign_path.clone();
    let previous = Manifest {
      store_dir:     old_store,
      secrets_dir:   dir.path().join("old.d"),
      templates_dir: dir.path().join("old.t"),
      state_file:    None,
      identities:    vec![],
      plugin:        None,
      secrets:       vec![token.clone(), foreign],
      templates:     vec![],
    };
    let new_path = dir.path().join("custom/new");
    token.path = new_path;
    let next = Manifest {
      store_dir: dir.path().join("new-store"),
      secrets: vec![token],
      ..previous.clone()
    };
    prune_previous_outputs(&previous, &next).expect("prune old outputs");
    assert!(fs::symlink_metadata(old_path).is_err());
    assert_eq!(
      fs::read_link(foreign_path).expect("unmanaged link retained"),
      PathBuf::from("/unrelated")
    );
    retire_previous_store(
      &previous,
      &next,
      &old_generation.join("manifest.json"),
    )
    .expect("retire old current pointer");
    assert!(fs::symlink_metadata(previous.store_dir.join("current")).is_err());
    symlink("/unrelated", previous.store_dir.join("current"))
      .expect("foreign current pointer");
    retire_previous_store(
      &previous,
      &next,
      &old_generation.join("manifest.json"),
    )
    .expect("leave foreign pointer");
    assert_eq!(
      fs::read_link(previous.store_dir.join("current"))
        .expect("foreign pointer kept"),
      PathBuf::from("/unrelated")
    );
  }
}
