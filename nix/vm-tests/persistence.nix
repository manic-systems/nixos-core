{
  lib,
  mkTest,
  nixosModule,
  testCommons,
  util-linux,
}: let
  inherit (lib.modules) mkForce;
  inherit (lib.meta) getExe';
in
  mkTest {
    name = "nixos-core-persistence";

    nodes = let
      common = {config, ...}: {
        imports = [nixosModule testCommons];

        boot.loader.grub.enable = false;
        environment.systemPackages = [config.system.nixos-core.package];
        system.nixos-core = {
          enable = true;
          persistence = {
            enable = true;
            stores."/persist" = {
              commonMountOptions = ["exec" "noexec" "x-gvfs-hide"];
              entries = [
                {
                  target = "/var/lib/core-state";
                  owner = "root";
                  group = "root";
                  mode = "2750";
                  mountOptions = ["exec"];
                }
                {
                  target = "/var/lib/core-state/restricted";
                  mountOptions = ["noexec" "nosymfollow"];
                }
                {
                  target = "/var/lib/core-state/readonly";
                  kind = "file";
                  mountOptions = ["ro"];
                }
                {
                  target = "/srv/core-state";
                  manageMetadata = false;
                  mountOptions = ["noexec"];
                }
                {
                  target = "/etc/core-id";
                  kind = "file";
                  method = "symlink";
                }
              ];
              # A file persisted inside a persisted directory, declared through
              # the attribute set form both lists coerce to.
              users.bob = {
                directories = [".local/share/app"];
                files = [
                  {
                    target = ".local/share/app/plugin.so";
                    mountOptions = ["exec"];
                  }
                ];
              };

              users.alice = [
                ".ssh"
                {
                  target = ".local/state/core";
                  mode = "0700";
                }
                {
                  target = ".config/core/settings";
                  kind = "file";
                  method = "symlink";
                  parent.enable = true;
                  parent.mode = "0700";
                }
              ];
            };

            # Nested below the /persist bind so reloads that replace the
            # parent have to unmount and re-project it.
            stores."/cache".entries = ["/var/lib/core-state/nested"];
          };
        };

        users.users = {
          alice = {
            isNormalUser = true;
            group = "users";
          };

          bob = {
            isNormalUser = true;
            group = "users";
          };
        };

        # Narrows one bind to a child, turns a bind into a symlink and moves a
        # symlink source, which a reload has to apply on top of the live
        # projections.
        specialisation.changed.configuration = {
          system.nixos-core.persistence.stores."/persist".entries = lib.mkForce [
            {
              target = "/var/lib/core-state/sub";
            }
            {
              target = "/srv/core-state";
              method = "symlink";
              manageMetadata = false;
            }
            {
              target = "/etc/core-id";
              source = "etc/core-id-renamed";
              kind = "file";
              method = "symlink";
            }
          ];
        };

        systemd.services.persistence-consumer = {
          wantedBy = ["multi-user.target"];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${getExe' util-linux "mountpoint"} -q /var/lib/core-state";
          };
        };

        specialisation.visible.configuration = {
          system.nixos-core.persistence.stores."/persist".commonMountOptions = lib.mkForce [];
        };

        virtualisation = {
          emptyDiskImages = [128 128 128];
          # qemu-vm.nix replaces fileSystems, so this test supplies the mount
          # we need
          fileSystems = {
            "/persist" = {
              device = "/dev/vdb";
              fsType = "ext4";
              neededForBoot = true;
              options = ["nosuid" "nodev"];
            };

            "/srv" = {
              device = "/dev/vdc";
              fsType = "ext4";
              neededForBoot = true;
            };

            "/cache" = {
              device = "/dev/vdd";
              fsType = "ext4";
              neededForBoot = true;
            };
          };
        };
      };
    in {
      scripted = {
        imports = [common];
        networking.hostId = "8badf00d";
        boot.initrd = {
          systemd.enable = false;
          postDeviceCommands = ''
            for device in /dev/vdb /dev/vdc /dev/vdd; do
              if ! blkid "$device" >/dev/null 2>&1; then
                mke2fs -F "$device"
              fi
            done
          '';
        };
      };

      # XXX: We'll also want to test commonly ephemeral filesystems. *Most* people
      # seem to prefer ZFS pools and BTRFS snapshots a la "erase your darlings" so
      # I'll test them instead of the less often (ab)used tmpfs path, which is silly.
      # We could even consider simply not supporting tmpfs tbh?
      btrfs = {
        imports = [common];
        boot.initrd.systemd.enable = true;
        networking.hostId = "b7f50001";
        virtualisation.fileSystems = {
          "/persist" = {
            device = mkForce "/dev/vdb";
            fsType = mkForce "btrfs";
            options = ["x-systemd.makefs" "nosymfollow"];
          };

          "/srv".options = ["x-systemd.makefs"];
          "/cache".options = ["x-systemd.makefs"];
        };
      };

      bcachefs = {
        imports = [common];
        boot = {
          supportedFilesystems = ["bcachefs"];
          initrd = {
            systemd.enable = false;
            postDeviceCommands = ''
              if ! bcachefs show-super /dev/vdb >/dev/null 2>&1; then
                bcachefs format --force /dev/vdb
              fi
              for device in /dev/vdc /dev/vdd; do
                if ! blkid "$device" >/dev/null 2>&1; then
                  mke2fs -F "$device"
                fi
              done
            '';
          };
        };

        virtualisation.fileSystems."/persist".fsType = mkForce "bcachefs";
      };

      zfs = {config, ...}: {
        imports = [common];
        networking.hostId = "2f5a0001";
        boot.supportedFilesystems = ["zfs"];

        virtualisation.fileSystems = {
          "/persist" = {
            device = mkForce "persist";
            fsType = mkForce "zfs";
            neededForBoot = mkForce false;
          };

          "/srv".options = ["x-systemd.makefs"];
          "/cache".options = ["x-systemd.makefs"];
        };

        systemd.services.zfs-create-persist = {
          requiredBy = ["zfs-import-persist.service"];
          before = ["zfs-import-persist.service"];
          after = ["systemd-modules-load.service"];
          unitConfig.DefaultDependencies = false;
          path = [config.boot.zfs.package];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            if zpool import 2>/dev/null | grep -q 'pool: persist'; then
              zpool import -N persist
            else
              zpool create -f -O mountpoint=legacy persist /dev/vdb
            fi
          '';
        };
      };

      systemd = {
        imports = [common];
        boot.initrd.systemd.enable = true;
        networking.hostId = "cafef00d";
        virtualisation.fileSystems = {
          "/persist".options = ["x-systemd.makefs"];
          "/srv".options = ["x-systemd.makefs"];
          "/cache".options = ["x-systemd.makefs"];
        };
      };
    };

    testScript = /* py */ ''
      # Oh lawd he testin.
      import json
      import shlex

      def mount_options(machine, target):
          return set(machine.succeed(f"findmnt --mtab --noheadings --output OPTIONS --mountpoint {target}").strip().split(","))

      def check_mount_options(machine):
          parent = mount_options(machine, "/var/lib/core-state")
          inherited = mount_options(machine, "/persist") & {"nosuid", "nodev", "nosymfollow"}
          assert inherited | {"x-gvfs-hide"} <= parent, parent
          assert "noexec" not in parent, parent
          assert "noexec" in mount_options(machine, "/srv/core-state")
          assert {"noexec", "nosymfollow"} <= mount_options(machine, "/var/lib/core-state/restricted")
          assert "ro" in mount_options(machine, "/var/lib/core-state/readonly")
          nested_file = mount_options(machine, "/home/bob/.local/share/app/plugin.so")
          assert "noexec" not in nested_file, nested_file
          assert "noexec" in mount_options(machine, "/home/bob/.local/share/app")
          plugin = "/persist/home/bob/.local/share/app/plugin.so"
          machine.succeed(f"echo '#!/bin/sh' > {plugin}", f"echo 'exit 0' >> {plugin}", f"chmod 755 {plugin}")
          machine.succeed("/home/bob/.local/share/app/plugin.so")
          machine.succeed("cp -L /run/current-system/sw/bin/true /var/lib/core-state/restricted/true")
          machine.fail("/var/lib/core-state/restricted/true")
          machine.fail("echo changed > /var/lib/core-state/readonly")
          machine.succeed("echo source-writable > /persist/var/lib/core-state/readonly")

      def check_mount_failure(machine):
          binary = machine.succeed("command -v persist").strip()
          root = "/tmp/persistence-options-test"
          machine.succeed(f"mkdir -p {root}/persist")
          machine.succeed(f"mount -t tmpfs tmpfs {root}/persist")
          previous = dict(store="/persist", source="/persist/previous", target="/target", kind="directory", method="bind")
          current = dict(previous, source="/persist/current", mountOptions=["x-gvfs-hide"])
          for name, entry in [("previous", previous), ("current", current)]:
              plan = shlex.quote(json.dumps(dict(version=1, entries=[entry])))
              machine.succeed(f"printf %s {plan} > {root}/{name}.json")
          machine.fail(f"env PATH=/missing {binary} --root {root} {root}/current.json")
          machine.fail(f"mountpoint -q {root}/target")
          machine.fail(f"test -e {root}/target")
          machine.fail(f"test -e {root}/run/nixos-core/persistence.json")
          machine.succeed(f"{binary} --root {root} {root}/previous.json")
          machine.succeed(f"echo previous > {root}/target/value")
          machine.fail(f"env PATH=/missing {binary} --root {root} {root}/current.json")
          machine.succeed(f"grep -qx previous {root}/target/value")
          recorded = json.loads(machine.succeed(f"cat {root}/run/nixos-core/persistence.json"))
          assert recorded["entries"][0]["source"] == previous["source"], recorded
          machine.succeed(f"{binary} --root {root} --clear")
          machine.succeed(f"umount {root}/persist")

      def exercise(machine):
          machine.start()
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("nixos-core-persistence.service")
          machine.wait_for_unit("persistence-consumer.service")

          machine.succeed("mountpoint -q /var/lib/core-state")
          machine.succeed("mountpoint -q /var/lib/core-state/nested")
          machine.succeed("mountpoint -q /srv/core-state")
          machine.succeed("mountpoint -q /home/alice/.local/state/core")
          machine.succeed("mountpoint -q /home/bob/.local/share/app/plugin.so")
          machine.succeed("test -f /persist/home/bob/.local/share/app/plugin.so")
          check_mount_options(machine)
          check_mount_failure(machine)
          machine.succeed("test $(stat -c %a /var/lib/core-state) = 2750")
          machine.succeed("test $(stat -c %a /home/alice/.ssh) = 700")
          machine.succeed("test $(stat -c %u /persist/home/alice/.local/state/core) -eq $(id -u alice)")
          machine.succeed("test $(stat -c %g /persist/home/alice/.local/state/core) -eq $(id -g alice)")
          machine.succeed("test $(stat -c %a /home/alice/.config/core) = 700")
          machine.succeed("test $(stat -c %u /home/alice/.config/core) -eq $(id -u alice)")
          for created in ["/home/alice/.config", "/home/alice/.local", "/home/alice/.local/state", "/persist/home/alice"]:
              machine.succeed(f"test $(stat -c %u {created}) -eq $(id -u alice)")
          machine.succeed("test $(stat -c %a /persist/home/alice) = $(stat -c %a /home/alice)")
          machine.succeed("test -L /etc/core-id")
          machine.succeed("test $(readlink /etc/core-id) = /persist/etc/core-id")
          machine.succeed("test -L /home/alice/.config/core/settings")

          machine.succeed("printf system-state > /var/lib/core-state/value")
          machine.succeed("printf nested-state > /var/lib/core-state/nested/value")
          machine.succeed("printf srv-state > /srv/core-state/value")
          machine.succeed("printf identity > /etc/core-id")
          machine.succeed("printf settings > /home/alice/.config/core/settings")
          machine.succeed("printf home-state > /home/alice/.local/state/core/value")
          machine.succeed("grep -qx system-state /persist/var/lib/core-state/value")
          machine.succeed("grep -qx nested-state /cache/var/lib/core-state/nested/value")
          machine.succeed("grep -qx srv-state /persist/srv/core-state/value")
          machine.succeed("grep -qx home-state /persist/home/alice/.local/state/core/value")

          machine.succeed("/run/current-system/specialisation/visible/bin/switch-to-configuration test")
          assert "x-gvfs-hide" not in mount_options(machine, "/var/lib/core-state")
          machine.succeed("/run/booted-system/bin/switch-to-configuration test")
          check_mount_options(machine)

          # Reloading a changed plan must replace projections that share a
          # target with the old ones, then switching back must undo that.
          machine.succeed("/run/current-system/specialisation/changed/bin/switch-to-configuration test")
          machine.succeed("systemctl is-active nixos-core-persistence.service")
          machine.succeed("mountpoint -q /var/lib/core-state/sub")
          machine.succeed("mountpoint -q /var/lib/core-state/nested")
          machine.succeed("grep -qx nested-state /var/lib/core-state/nested/value")
          machine.fail("mountpoint -q /var/lib/core-state")
          machine.succeed("test -L /srv/core-state")
          machine.succeed("grep -qx srv-state /srv/core-state/value")
          machine.succeed("test $(readlink /etc/core-id) = /persist/etc/core-id-renamed")
          machine.succeed("/run/booted-system/bin/switch-to-configuration test")
          machine.succeed("mountpoint -q /var/lib/core-state")
          machine.fail("test -L /srv/core-state")
          machine.succeed("mountpoint -q /srv/core-state")
          machine.succeed("mountpoint -q /var/lib/core-state/nested")
          check_mount_options(machine)
          machine.succeed("grep -qx nested-state /var/lib/core-state/nested/value")
          machine.fail("mountpoint -q /var/lib/core-state/sub")
          machine.succeed("test $(readlink /etc/core-id) = /persist/etc/core-id")
          machine.succeed("grep -qx identity /etc/core-id")

          # A busy projection must stop cleanup. Lazy unmounting would make this
          # appear to succeed while the process kept a hidden copy alive.
          machine.succeed("systemd-run --unit=core-holder --property=WorkingDirectory=/var/lib/core-state sleep 300")
          machine.succeed("systemctl stop nixos-core-persistence.service")
          machine.succeed("systemctl is-failed nixos-core-persistence.service")
          machine.succeed("mountpoint -q /var/lib/core-state")
          machine.succeed("mountpoint -q /srv/core-state")
          machine.succeed("mountpoint -q /home/alice/.local/state/core")
          machine.succeed("test -L /etc/core-id")
          machine.succeed("test -L /home/alice/.config/core/settings")
          machine.succeed("systemctl stop core-holder.service")
          machine.succeed("systemctl reset-failed nixos-core-persistence.service")
          machine.succeed("systemctl start nixos-core-persistence.service")

          # A clean stop removes all recorded projections. This is the same
          # ExecStop path used when a rebuilt system disables persistence.
          machine.succeed("systemctl stop nixos-core-persistence.service")
          machine.fail("mountpoint -q /var/lib/core-state")
          machine.fail("findmnt -n /var/lib/core-state/nested")
          machine.fail("mountpoint -q /srv/core-state")
          machine.fail("mountpoint -q /home/alice/.local/state/core")
          machine.fail("findmnt -n /home/bob/.local/share/app/plugin.so")
          machine.fail("test -e /etc/core-id")
          machine.fail("test -e /home/alice/.config/core/settings")
          assert "x-gvfs-hide" not in machine.succeed("cat /run/mount/utab")

          # A failed state commit must undo projections made by this invocation.
          machine.succeed("mkdir /run/nixos-core/persistence.json.new")
          machine.fail("systemctl start nixos-core-persistence.service")
          machine.fail("mountpoint -q /var/lib/core-state")
          machine.fail("findmnt -n /var/lib/core-state/nested")
          machine.fail("mountpoint -q /srv/core-state")
          machine.fail("mountpoint -q /home/alice/.local/state/core")
          machine.fail("test -e /etc/core-id")
          machine.fail("test -e /home/alice/.config/core/settings")
          assert "x-gvfs-hide" not in machine.succeed("cat /run/mount/utab")
          machine.succeed("rmdir /run/nixos-core/persistence.json.new")
          machine.succeed("systemctl reset-failed nixos-core-persistence.service")
          machine.succeed("systemctl start nixos-core-persistence.service")

          machine.shutdown()
          machine.start()
          machine.wait_for_unit("multi-user.target")

          machine.succeed("mountpoint -q /var/lib/core-state")
          machine.succeed("mountpoint -q /var/lib/core-state/nested")
          machine.succeed("mountpoint -q /srv/core-state")
          machine.succeed("mountpoint -q /home/alice/.local/state/core")
          machine.succeed("mountpoint -q /home/bob/.local/share/app/plugin.so")
          machine.succeed("grep -qx system-state /var/lib/core-state/value")
          machine.succeed("grep -qx nested-state /var/lib/core-state/nested/value")
          machine.succeed("grep -qx srv-state /srv/core-state/value")
          check_mount_options(machine)
          machine.succeed("grep -qx identity /etc/core-id")
          machine.succeed("grep -qx settings /home/alice/.config/core/settings")
          machine.succeed("grep -qx home-state /home/alice/.local/state/core/value")

      with subtest("scripted initrd"):
          exercise(scripted)

      with subtest("Btrfs persistence store"):
          exercise(btrfs)

      with subtest("bcachefs persistence store"):
          exercise(bcachefs)

      with subtest("ZFS persistence store"):
          exercise(zfs)

      with subtest("systemd initrd"):
          exercise(systemd)
    '';
  }
