{
  mkTest,
  nixosModule,
  testCommons,
}:
mkTest {
  name = "nixos-core-boot";

  nodes = {
    machine = {
      imports = [nixosModule testCommons];
      system.nixos-core.enable = true;
      boot = {
        loader.grub.enable = false;

        # Marker written by stage2's postBootCommands hook.
        postBootCommands = "touch /etc/post-boot-ran";
      };

      networking.hostId = "cafebabe";
    };

    # VM with strict activation mode enabled: stage2 must fail when activate is
    # absent. In a standard NixOS system the activate script is always present,
    # so this node should boot successfully, which tells us that strict mode
    # does not break a normal boot.
    strictActivation = {
      imports = [nixosModule testCommons];
      system.nixos-core = {
        enable = true;
        strictActivation = true;
      };
      boot.loader.grub.enable = false;

      networking.hostId = "00c0ffee";
    };
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")

    with subtest("basic presence"):
      machine.succeed("test -f /etc/os-release")
      machine.succeed("test -e /etc/passwd")

    with subtest("stage2 system symlinks point into the store"):
      machine.succeed("readlink /run/current-system | grep -q '^/nix/store/'")
      machine.succeed("readlink /run/booted-system  | grep -q '^/nix/store/'")
      machine.succeed("test /run/current-system -ef /run/booted-system")

    with subtest("nix store mount options"):
      for opt in ["ro", "nosuid", "nodev"]:
        machine.succeed(f'[[ "$(findmnt --direction backward --first-only --noheadings --output OPTIONS /nix/store)" =~ (^|,){opt}(,|$) ]]')
      machine.fail("touch /nix/store/should-not-work")

    with subtest("postBootCommands ran"):
      machine.succeed("test -f /etc/post-boot-ran")

    with subtest("HOST_ID written as 4 native-endian bytes"):
      machine.succeed("test -f /etc/hostid")
      machine.succeed("test $(wc -c < /etc/hostid) -eq 4")
      machine.succeed("od -An -tx1 /etc/hostid | tr -d ' \\n' | grep -qx 'bebafeca'")

    with subtest("stage1 wipes environment before exec /init"):
      # Upstream pivots with `exec env -i` so LD_LIBRARY_PATH=@extraUtils@/lib
      # doesn't leak into PID 1 and break systemd's libseccomp dlopen.
      machine.fail("tr '\\0' '\\n' < /proc/1/environ | grep -q '^LD_LIBRARY_PATH='")

      # Without seccomp, systemd drops the service PATH that resolves
      # relative ExecStart names, so tmpfiles-setup and friends 203/EXEC.
      machine.succeed("test -z \"$(systemctl --failed --no-legend)\"")

    # With STAGE2_STRICT_ACTIVATION=true and an activate script present (normal
    # NixOS), stage2 must not abort. If it did, the VM would never reach
    # multi-user.target.
    strictActivation.start()
    strictActivation.wait_for_unit("multi-user.target")

    with subtest("strict activation - boots normally when activate script is present"):
      # Verify the option was threaded through to the stage-2 init script: the
      # env var export must be present in the script that was actually used for
      # this boot.
      strictActivation.succeed("grep -q 'STAGE2_STRICT_ACTIVATION=true' $(readlink /run/current-system)/prepare-root")
  '';
}
