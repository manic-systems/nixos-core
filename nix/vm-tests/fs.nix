{
  mkTest,
  nixosModule,
  testCommons,
}:
let
  bcachefsUuid = "fef71188-998b-4a00-a263-6b525fe9832b";
in
mkTest {
  name = "nixos-core-fs";

  nodes.machine = {
    imports = [nixosModule testCommons];
    system.nixos-core.enable = true;

    boot = {
      loader.grub.enable = false;
      supportedFilesystems = ["bcachefs"];

      initrd.systemd.services = {
        bcachefs-format = {
          description = "Format the test bcachefs filesystem";
          requiredBy = ["sysroot-mnt-bcache3.mount"];
          before = ["sysroot-mnt-bcache3.mount"];
          wants = ["dev-vdb.device" "dev-vdc.device" "dev-vdd.device"];
          after = ["dev-vdb.device" "dev-vdc.device" "dev-vdd.device"];
          unitConfig.DefaultDependencies = false;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            if ! bcachefs show-super /dev/vdb >/dev/null 2>&1; then
              bcachefs format --force \
                --uuid=${bcachefsUuid} \
                --metadata_replicas 3 \
                --label=disk0 /dev/vdb \
                --label=disk1 /dev/vdc \
                --label=disk2 /dev/vdd
            fi
          '';
        };
        unlock-bcachefs-mnt-bcache3.enable = false;
      };
    };

    virtualisation.emptyDiskImages = [
      512
      512
      512
    ];

    # qemu-vm.nix replaces fileSystems via mkVMOverride.
    virtualisation.fileSystems."/mnt/bcache3" = {
      device = "UUID=${bcachefsUuid}";
      fsType = "bcachefs";
      neededForBoot = true;
      options = ["noatime"];
    };
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")

    with subtest("three-device bcachefs mounts"):
      machine.succeed("mountpoint -q /mnt/bcache3")
      fstype = machine.succeed("findmnt -n -o FSTYPE /mnt/bcache3").strip()
      assert fstype == "bcachefs", f"expected bcachefs, got {fstype!r}"

    with subtest("all three members attached"):
      usage = machine.succeed("bcachefs fs usage /mnt/bcache3")
      for dev in ["vdb", "vdc", "vdd"]:
        assert dev in usage, f"{dev} missing from usage:\n{usage}"

    with subtest("noatime forwarded"):
      opts = machine.succeed("findmnt -n -o OPTIONS /mnt/bcache3").strip()
      assert "noatime" in opts, f"options: {opts!r}"

    with subtest("filesystem is writable and survives reboot"):
      machine.succeed("echo hello > /mnt/bcache3/marker")
      machine.shutdown()
      machine.start()
      machine.wait_for_unit("multi-user.target")
      machine.succeed("mountpoint -q /mnt/bcache3")
      machine.succeed("grep -qx hello /mnt/bcache3/marker")
  '';
}
