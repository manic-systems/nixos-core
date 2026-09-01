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
      common = {
        imports = [nixosModule testCommons];

        boot.loader.grub.enable = false;
        system.nixos-core = {
          enable = true;
          persistence = {
            enable = true;
            stores."/persist" = {
              commonMountOptions = ["x-gvfs-hide"];
              entries = [
                {
                  target = "/var/lib/core-state";
                  owner = "root";
                  group = "root";
                  mode = "2750";
                }
                {
                  target = "/srv/core-state";
                  manageMetadata = false;
                  mountOptions = ["exec"];
                }
                {
                  target = "/etc/core-id";
                  kind = "file";
                  method = "symlink";
                }
              ];
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

        users.users.alice = {
          isNormalUser = true;
          group = "users";
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

        virtualisation = {
          emptyDiskImages = [128 128 128];
          # qemu-vm.nix replaces fileSystems, so this test supplies the mount
          # we need
          fileSystems = {
            "/persist" = {
              device = "/dev/vdb";
              fsType = "ext4";
              neededForBoot = true;
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
            options = ["x-systemd.makefs"];
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

    testScript = ''
      # Oh lawd he testin.
      def exercise(machine):
          machine.start()
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("nixos-core-persistence.service")
          machine.wait_for_unit("persistence-consumer.service")

          machine.succeed("mountpoint -q /var/lib/core-state")
          machine.succeed("mountpoint -q /var/lib/core-state/nested")
          machine.succeed("mountpoint -q /srv/core-state")
          machine.succeed("mountpoint -q /home/alice/.local/state/core")
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
          machine.fail("test -e /etc/core-id")
          machine.fail("test -e /home/alice/.config/core/settings")

          # A failed state commit must undo projections made by this invocation.
          machine.succeed("mkdir /run/nixos-core/persistence.json.new")
          machine.fail("systemctl start nixos-core-persistence.service")
          machine.fail("mountpoint -q /var/lib/core-state")
          machine.fail("findmnt -n /var/lib/core-state/nested")
          machine.fail("mountpoint -q /srv/core-state")
          machine.fail("mountpoint -q /home/alice/.local/state/core")
          machine.fail("test -e /etc/core-id")
          machine.fail("test -e /home/alice/.config/core/settings")
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
          machine.succeed("grep -qx system-state /var/lib/core-state/value")
          machine.succeed("grep -qx nested-state /var/lib/core-state/nested/value")
          machine.succeed("grep -qx srv-state /srv/core-state/value")
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
