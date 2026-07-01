use std::{
  collections::HashSet,
  fs,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use log::warn;

use crate::DirectSymlink;

/// Diff source for smfh activation.
///
/// Old nixos-core releases tracked direct symlinks in the smfh manifest. The
/// current activation path owns those entries separately, so smfh must not
/// deactivate them while migrating old state.
pub(crate) enum DiffBase {
  Original(PathBuf),
  Filtered(PathBuf),
}

impl DiffBase {
  pub(crate) fn prepare(
    manifest_path: &Path,
    direct_symlinks: &[DirectSymlink],
  ) -> Result<Self> {
    let Some(filtered) = filtered_old_manifest(manifest_path, direct_symlinks)?
    else {
      return Ok(Self::Original(manifest_path.to_path_buf()));
    };

    let path = PathBuf::from(format!(
      "{}.diff-base.{}",
      manifest_path.display(),
      std::process::id()
    ));
    fs::write(&path, filtered).with_context(|| {
      format!("Failed to write manifest diff base {}", path.display())
    })?;

    Ok(Self::Filtered(path))
  }

  pub(crate) fn path(&self) -> &Path {
    match self {
      Self::Original(path) | Self::Filtered(path) => path,
    }
  }
}

impl Drop for DiffBase {
  fn drop(&mut self) {
    if let Self::Filtered(path) = self {
      let _ = fs::remove_file(path);
    }
  }
}

fn filtered_old_manifest(
  manifest_path: &Path,
  direct_symlinks: &[DirectSymlink],
) -> Result<Option<String>> {
  if direct_symlinks.is_empty() {
    return Ok(None);
  }

  let contents = match fs::read_to_string(manifest_path) {
    Ok(contents) => contents,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(e) => {
      return Err(e).with_context(|| {
        format!("Failed to read {}", manifest_path.display())
      });
    },
  };

  let mut value: serde_json::Value = match serde_json::from_str(&contents) {
    Ok(value) => value,
    Err(e) => {
      warn!(
        "failed to parse old manifest {} while preparing diff base: {e}",
        manifest_path.display()
      );
      return Ok(None);
    },
  };

  let Some(files) = value
    .get_mut("files")
    .and_then(|files| files.as_array_mut())
  else {
    return Ok(None);
  };

  let direct_targets: HashSet<PathBuf> = direct_symlinks
    .iter()
    .map(|link| link.target.clone())
    .collect();
  let original_len = files.len();
  files.retain(|file| {
    file
      .get("target")
      .and_then(|target| target.as_str())
      .is_none_or(|target| !direct_targets.contains(Path::new(target)))
  });

  if files.len() == original_len {
    return Ok(None);
  }

  Ok(Some(serde_json::to_string_pretty(&value)?))
}

#[cfg(test)]
mod tests {
  use tempfile::TempDir;

  use super::*;

  fn write_manifest(path: &Path, targets: &[&str]) {
    let files = targets
      .iter()
      .map(|target| {
        serde_json::json!({
          "source": "/etc/static/source",
          "target": target,
          "kind": "Symlink",
          "clobber": true
        })
      })
      .collect::<Vec<_>>();
    fs::write(
      path,
      serde_json::json!({"version": 1, "files": files}).to_string(),
    )
    .unwrap();
  }

  fn direct_symlink(target: &str) -> DirectSymlink {
    DirectSymlink {
      source: PathBuf::from("/proc/mounts"),
      target: PathBuf::from(target),
    }
  }

  fn manifest_targets(contents: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(contents).unwrap();
    value["files"]
      .as_array()
      .unwrap()
      .iter()
      .map(|file| file["target"].as_str().unwrap().to_string())
      .collect()
  }

  #[test]
  fn prunes_current_direct_symlink_targets() {
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("etc-manifest.json");
    write_manifest(&manifest_path, &["/etc/mtab", "/etc/hostname"]);

    let filtered =
      filtered_old_manifest(&manifest_path, &[direct_symlink("/etc/mtab")])
        .unwrap()
        .unwrap();

    assert_eq!(manifest_targets(&filtered), vec!["/etc/hostname"]);
    assert_eq!(
      manifest_targets(&fs::read_to_string(&manifest_path).unwrap()),
      vec!["/etc/mtab", "/etc/hostname"]
    );
  }

  #[test]
  fn reuses_manifest_without_direct_symlink_targets() {
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("etc-manifest.json");
    write_manifest(&manifest_path, &["/etc/hostname"]);

    let filtered =
      filtered_old_manifest(&manifest_path, &[direct_symlink("/etc/mtab")])
        .unwrap();

    assert!(filtered.is_none());
  }
}
