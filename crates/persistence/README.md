# persistence

[Impermanence]: https://github.com/nix-community/impermanence

The persistence component is an alternative to the popular [Impermanence]
project, which could be considered the prior art for this component, that
projects selected files and directories from one or more mounted stores into the
live root.

The implementation uses bind mounts or symlinks. It validates the plan before
removing old projections and restores the recorded plan if a new projection or
state commit fails. It resolves plan paths from an open root directory, rejects
intermediate symlinks, and pins bind sources and targets by file descriptor. A
stale bind mount is removed only when its target still resolves to the recorded
source. Busy mounts make reconciliation fail; the utility never uses lazy
unmounting.

> [!IMPORTANT]
> nixos-core does not implement root cleanup. It's left out as a separate
> policy, because there are various differing (dare I say competing) ways of
> implementing. You may pick whichever path, e.g., tmpfs or ZFS datasets at your
> own discretion.

## Requirements

A store must already be mounted. Missing source objects and target parents are
created. Ownership and mode changes are optional so stores without Unix
ownership or permission semantics can still be used. File persistence needs a
writable parent directory when applications update files with atomic rename.
Persist the containing directory when that is the application's normal update
model.

## NixOS module

The NixOS module runs the persistence component after local filesystems and
before `sysinit.target`, so ordinary services observe the projected paths. The
unit reapplies changed plans and clears recorded projections when it stops. See
this example below:

```nix
{
  fileSystems."/persist" = {
    device = "/dev/disk/by-label/persist";
    fsType = "xfs";
  };

  system.nixos-core.persistence = {
    enable = true;
    stores."/persist" = {
      entries = [
        "/var/lib/nixos"
        {
          target = "/etc/machine-id";
          kind = "file";
          method = "symlink";
        }
      ];
      users.alice = [".ssh" ".local/state"];
    };
  };
}
```

The service waits for each store and target mount path. It does not change
`fileSystems` or require `neededForBoot`, so its ordering works with both
scripted and systemd stage 1. Directories use bind mounts by default. Files may
use bind mounts or symlinks. Each expanded entry also accepts `source`,
`owner`, `group`, `mode`, `manageMetadata`, and a `parent` metadata submodule.
User entries are relative to the configured home. User directories default to
mode `0700`; system directories default to `0755`.

You should set `manageMetadata = false` for a store that does not implement Unix
ownership and modes. This is unlikely, but we'd rather avoid assuming. Parent
metadata is opt-in with `parent.enable = true`; it then defaults to the entry's
owner and group, and to `0700` for user entries or `0755` for system entries.
For file entries, `parent` controls the source and target parent directories.
Applications that replace files with atomic rename need a writable parent.
Persisting the whole containing directory is usually the correct entry. Existing
bind targets are hidden, not copied. Symlinks never replace existing targets.
Move existing data into the store before enabling an entry.
