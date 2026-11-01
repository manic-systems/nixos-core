{
  mkTest,
  nixosModule,
  testCommons,
}:
let
  bcachefsUuid = "fef71188-998b-4a00-a263-6b525fe9832b";
in
mkTest {
  name = "nixos-core-bcachefs";

  nodes.machine = {
    imports = [
      nixosModule
      testCommons
    ];
    system.nixos-core.enable = true;

    boot = {
      loader.grub.enable = false;
      supportedFilesystems = [ "bcachefs" ];

      initrd.systemd.services = {
        bcachefs-format = {
          description = "Format the test bcachefs filesystem";
          requiredBy = [ "sysroot-mnt-bcache.mount" ];
          before = [ "sysroot-mnt-bcache.mount" ];
          wants = [ "dev-vdb.device" "dev-vdc.device" ];
          after = [ "dev-vdb.device" "dev-vdc.device" ];
          unitConfig.DefaultDependencies = false;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            if ! bcachefs show-super /dev/vdb >/dev/null 2>&1; then
              bcachefs format --force \
                --uuid=${bcachefsUuid} \
                --metadata_replicas 2 \
                --label=ssd /dev/vdb \
                --label=hdd /dev/vdc
            fi
          '';
        };
        unlock-bcachefs-mnt-bcache.enable = false;
      };
    };

    virtualisation.emptyDiskImages = [
      1024
      1024
    ];

    # qemu-vm.nix replaces `fileSystems` via mkVMOverride, meaning a plain
    # fileSystems entry gets dropped. As such, we mount it via virtualisation.
    virtualisation.fileSystems."/mnt/bcache" = {
      device = "UUID=${bcachefsUuid}";
      fsType = "bcachefs";
      neededForBoot = true;
      options = [ "noatime" ];
    };
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")

    with subtest("mount.bcachefs is in extraUtils"):
      machine.succeed("test -x /run/current-system/sw/bin/mount.bcachefs")

    with subtest("/mnt/bcache mounted as bcachefs"):
      machine.succeed("mountpoint -q /mnt/bcache")
      fstype = machine.succeed("findmnt -n -o FSTYPE /mnt/bcache").strip()
      assert fstype == "bcachefs", f"expected bcachefs, got {fstype!r}"

    with subtest("both members attached"):
      # mount(2) would have only taken the first device in the colon list.
      usage = machine.succeed("bcachefs fs usage /mnt/bcache")
      for dev in ["vdb", "vdc"]:
        assert dev in usage, f"{dev} missing:\n{usage}"

    with subtest("noatime forwarded"):
      opts = machine.succeed("findmnt -n -o OPTIONS /mnt/bcache").strip()
      assert "noatime" in opts, f"options: {opts!r}"

    with subtest("mount.bcachefs accepts UUID= (what mount_root passes)"):
      uuid = machine.succeed(
        "bcachefs show-super /dev/vdb | awk '/External UUID/ {print $3}'"
      ).strip()
      machine.succeed("umount /mnt/bcache")
      machine.succeed(f"mount.bcachefs -o noatime,rw 'UUID={uuid}' /mnt/bcache")
      machine.succeed("mountpoint -q /mnt/bcache")
      usage = machine.succeed("bcachefs fs usage /mnt/bcache")
      for dev in ["vdb", "vdc"]:
        assert dev in usage

    with subtest("existing multi-device bcachefs remounts across reboot"):
      machine.succeed("touch /mnt/bcache/marker")
      machine.shutdown()
      machine.start()
      machine.wait_for_unit("multi-user.target")
      machine.succeed("mountpoint -q /mnt/bcache")
      machine.succeed("test -f /mnt/bcache/marker")
  '';
}
