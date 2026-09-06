use std::{
  fs,
  os::fd::{AsRawFd, OwnedFd},
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use nix::mount::{MsFlags, mount};

use crate::{mount_id, proc_path};

#[derive(Debug)]
pub(crate) struct MountOptions {
  set:       MsFlags,
  clear:     MsFlags,
  userspace: Vec<String>,
}

impl MountOptions {
  pub(crate) fn parse(options: &[String]) -> Result<Self> {
    let mut parsed = Self {
      set:       MsFlags::empty(),
      clear:     MsFlags::empty(),
      userspace: Vec::new(),
    };
    for option in options.iter().flat_map(|value| value.split(',')) {
      let (mask, value) = match option {
        "defaults" => continue,
        "ro" => (MsFlags::MS_RDONLY, true),
        "rw" => (MsFlags::MS_RDONLY, false),
        "nosuid" => (MsFlags::MS_NOSUID, true),
        "suid" => (MsFlags::MS_NOSUID, false),
        "nodev" => (MsFlags::MS_NODEV, true),
        "dev" => (MsFlags::MS_NODEV, false),
        "noexec" => (MsFlags::MS_NOEXEC, true),
        "exec" => (MsFlags::MS_NOEXEC, false),
        "nosymfollow" => (nosymfollow(), true),
        "symfollow" => (nosymfollow(), false),
        "nodiratime" => (MsFlags::MS_NODIRATIME, true),
        "diratime" => (MsFlags::MS_NODIRATIME, false),
        "noatime" | "relatime" | "strictatime" => {
          let flag = match option {
            "noatime" => MsFlags::MS_NOATIME,
            "relatime" => MsFlags::MS_RELATIME,
            _ => MsFlags::MS_STRICTATIME,
          };
          parsed.set_flags(
            MsFlags::MS_NOATIME
              | MsFlags::MS_RELATIME
              | MsFlags::MS_STRICTATIME,
            flag,
          );
          continue;
        },
        "atime" => (MsFlags::MS_NOATIME, false),
        "norelatime" => (MsFlags::MS_RELATIME, false),
        "nostrictatime" => (MsFlags::MS_STRICTATIME, false),
        value if value.starts_with("x-") && value.len() > 2 => {
          ensure!(
            !value.chars().any(char::is_control),
            "Mount option {value:?} contains a control character"
          );
          parsed.userspace.push(value.to_owned());
          continue;
        },
        _ => bail!("Unsupported bind mount option {option:?}"),
      };
      parsed.set_flags(mask, if value { mask } else { MsFlags::empty() });
    }
    Ok(parsed)
  }

  fn set_flags(&mut self, mask: MsFlags, value: MsFlags) {
    self.set = self.set.difference(mask) | value;
    self.clear = (self.clear | mask).difference(value);
  }

  pub(crate) fn apply(&self, target: &OwnedFd) -> Result<()> {
    if self.set.is_empty() && self.clear.is_empty() {
      return Ok(());
    }
    let flags = existing_flags(target)?.difference(self.clear) | self.set;
    mount(
      None::<&str>,
      &proc_path(target),
      None::<&str>,
      MsFlags::MS_REMOUNT | MsFlags::MS_BIND | flags,
      None::<&str>,
    )
    .context("Failed to apply bind mount flags")
  }

  pub(crate) fn record(
    &self,
    source: &OwnedFd,
    target: &OwnedFd,
  ) -> Result<()> {
    if self.userspace.is_empty() {
      return Ok(());
    }
    run_bookkeeping(
      Command::new("mount")
        .args(["--fake", "--internal-only", "--options-source", "disable"])
        .args(["--options", &format!("bind,{}", self.userspace.join(","))])
        .arg("--source")
        .arg(proc_path_for_child(source))
        .arg("--target")
        .arg(proc_path_for_child(target)),
    )
  }

  pub(crate) fn forget(&self, target: &Path) -> Result<()> {
    if self.userspace.is_empty() {
      return Ok(());
    }
    run_bookkeeping(
      Command::new("umount")
        .args(["--fake", "--internal-only", "--no-canonicalize", "--"])
        .arg(target),
    )
  }
}

fn nosymfollow() -> MsFlags {
  MsFlags::from_bits_retain(libc::MS_NOSYMFOLLOW)
}

fn existing_flags(target: &OwnedFd) -> Result<MsFlags> {
  let id = mount_id(target).context("Failed to identify bind mount")?;
  let mounts = fs::read_to_string("/proc/self/mountinfo")
    .context("Failed to read mount flags")?;
  let options = mounts
    .lines()
    .find_map(|line| {
      let mut fields = line.split_whitespace();
      (fields.next()?.parse::<u64>().ok()? == id)
        .then(|| fields.nth(4))
        .flatten()
    })
    .context("Bind mount disappeared from mountinfo")?;
  Ok(mountinfo_flags(options))
}

fn mountinfo_flags(options: &str) -> MsFlags {
  let mut flags = MsFlags::empty();
  for option in options.split(',') {
    flags |= match option {
      "ro" => MsFlags::MS_RDONLY,
      "nosuid" => MsFlags::MS_NOSUID,
      "nodev" => MsFlags::MS_NODEV,
      "noexec" => MsFlags::MS_NOEXEC,
      "nosymfollow" => nosymfollow(),
      "noatime" => MsFlags::MS_NOATIME,
      "nodiratime" => MsFlags::MS_NODIRATIME,
      "relatime" => MsFlags::MS_RELATIME,
      _ => MsFlags::empty(),
    };
  }
  if !flags.intersects(MsFlags::MS_NOATIME | MsFlags::MS_RELATIME) {
    flags.insert(MsFlags::MS_STRICTATIME);
  }
  flags
}

fn proc_path_for_child(fd: &OwnedFd) -> PathBuf {
  PathBuf::from(format!(
    "/proc/{}/fd/{}",
    std::process::id(),
    fd.as_raw_fd()
  ))
}

fn run_bookkeeping(command: &mut Command) -> Result<()> {
  let output = command
    .output()
    .context("Failed to run libmount bookkeeping")?;
  ensure!(
    output.status.success(),
    "Failed to update libmount records ({})",
    String::from_utf8_lossy(&output.stderr).trim()
  );
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn entry_options_override_store_defaults() {
    let options =
      MountOptions::parse(&["exec,noexec".into(), "exec".into(), "rw".into()])
        .unwrap();
    let existing =
      mountinfo_flags("ro,nosuid,nodev,noexec,nosymfollow,relatime,idmapped");
    assert_eq!(
      existing.difference(options.clear) | options.set,
      MsFlags::MS_NOSUID
        | MsFlags::MS_NODEV
        | nosymfollow()
        | MsFlags::MS_RELATIME
    );
  }

  #[test]
  fn preserves_and_clears_nosymfollow() {
    let preserved = MountOptions::parse(&["nosymfollow,exec".into()]).unwrap();
    assert_eq!(preserved.set, nosymfollow());
    let cleared =
      MountOptions::parse(&["nosymfollow,symfollow".into()]).unwrap();
    assert!(cleared.set.is_empty());
    assert_eq!(cleared.clear, nosymfollow());
  }

  #[test]
  fn rejects_mount_operations_and_filesystem_options() {
    for option in [
      "move",
      "rbind",
      "remount",
      "size=1M",
      "",
      "X-mount.mkdir",
      "idmapped",
    ] {
      assert!(MountOptions::parse(&[option.into()]).is_err());
    }
    let options = MountOptions::parse(&["noexec,x-gvfs-hide".into()]).unwrap();
    assert_eq!(options.set, MsFlags::MS_NOEXEC);
    assert_eq!(options.userspace, ["x-gvfs-hide"]);
  }
}
