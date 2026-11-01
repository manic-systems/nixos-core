{
  mkTest,
  nixosModule,
  testCommons,
  writeText,
}: let
  agenix = builtins.fetchTarball {
    url = "https://github.com/ryantm/agenix/archive/b027ee29d959fda4b60b57566d64c98a202e0feb.tar.gz";
    sha256 = "1wlpvpj45qfixdzhmk2cgiwlkyaf8a5mjy2jp5lsx2wsxblclngm";
  };

  agenixModule = "${agenix}/modules/age.nix";

  testIdentity = writeText "agenix-test-ssh-host-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACB4c+mTJu+778KqOxFZbC2ixO5MsYfA+g+loPqZks/GrQAAAIjmPaEV5j2h
    FQAAAAtzc2gtZWQyNTUxOQAAACB4c+mTJu+778KqOxFZbC2ixO5MsYfA+g+loPqZks/GrQ
    AAAED4GzlhYb3Qz1HKMyESMgePExz7bquTI26zxEl9my5Ft3hz6ZMm77vvwqo7EVlsLaLE
    7kyxh8D6D6Wg+pmSz8atAAAAAAECAwQF
    -----END OPENSSH PRIVATE KEY-----
  '';

  testSecret = writeText "agenix-test-secret.age" ''
    -----BEGIN AGE ENCRYPTED FILE-----
    YWdlLWVuY3J5cHRpb24ub3JnL3YxCi0+IHNzaC1lZDI1NTE5IE5vemxsUSBDQ1lJ
    aGJaMzBtQXdXRVI3ek1sYWI4RnR2MWtZQVpMUGlIWU91NEZPTHpnCkF4VEdSSXky
    VTJSeHdxNis3OTN6cUE0bTZWNEF4amJlYVV1OTNHeFVaaXcKLS0tICttKzd6cWFt
    WTRhb1kvbjJBMXR1WkVPZEZVaUl2NjJmU1ZhaGg2eGtva0EKpwyuxq1FtV9eORZO
    nkdk1yg9NNhur0Mq9mUxnH4S57N2R0cF0D02RUFPkcbmJy/UwMFQG44H2dW6
    -----END AGE ENCRYPTED FILE-----
  '';

  agenixNode = initrdSystemd: {config, ...}: {
    imports = [testCommons nixosModule agenixModule];

    system.nixos-core.enable = true;
    boot.loader.grub.enable = false;

    system.activationScripts = {
      agenixHostKey = {
        text = ''
          install -Dm600 ${testIdentity} /etc/ssh/ssh_host_ed25519_key
        '';
        deps = ["specialfs"];
      };
      agenixInstall.deps = ["agenixHostKey"];
    };

    age = {
      identityPaths = ["/etc/ssh/ssh_host_ed25519_key"];
      secrets.integration = {
        file = testSecret;
        owner = "agenix-user";
        group = "agenix-group";
        mode = "0440";
      };
    };

    users = {
      groups.agenix-group.gid = 1500;
      users.agenix-user = {
        isSystemUser = true;
        uid = 1500;
        group = "agenix-group";
      };
    };

    systemd.services.agenix-consumer = {
      wantedBy = ["multi-user.target"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        grep -qxF nixos-core-agenix-secret ${config.age.secrets.integration.path}
      '';
    };
  };
in
  mkTest {
    name = "nixos-core-agenix";

    nodes = {
      scripted = agenixNode false;
      systemdStage1 = agenixNode true;
    };

    testScript = ''
      def check_agenix(machine):
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("agenix-consumer.service")

          with subtest(f"{machine.name}: decrypted secret content"):
              machine.succeed("grep -qxF nixos-core-agenix-secret /run/agenix/integration")

          with subtest(f"{machine.name}: secret ownership and mode"):
              machine.succeed("stat -c '%U:%G' /run/agenix/integration | grep -qx agenix-user:agenix-group")
              machine.succeed("stat -c '%a' /run/agenix/integration | grep -qx 440")

          with subtest(f"{machine.name}: generation link created"):
              machine.succeed("test -L /run/agenix")
              machine.succeed("readlink -f /run/agenix/integration | grep -q '^/run/agenix.d/'")

      scripted.start()
      check_agenix(scripted)

      systemdStage1.start()
      check_agenix(systemdStage1)
    '';
  }
