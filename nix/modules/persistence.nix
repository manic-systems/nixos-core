{
  config,
  pkgs,
  lib,
  ...
}: let
  inherit (builtins) toJSON;
  inherit (lib.options) mkOption mkEnableOption;
  inherit (lib.modules) mkIf;
  inherit (lib.types) attrsOf bool coercedTo enum listOf nullOr str strMatching submodule;
  inherit (lib.strings) hasInfix hasPrefix removePrefix removeSuffix;
  inherit (lib.attrsets) hasAttr attrNames mapAttrsToList;
  inherit (lib.lists) all concatLists filter unique any;
  inherit (lib.meta) getExe';

  cfg = config.system.nixos-core;
  persistenceCfg = cfg.persistence;

  parentType = submodule {
    options = {
      enable = mkOption {
        type = bool;
        default = false;
        description = "Whether to apply the entry metadata defaults to the source and target parent directories.";
      };

      owner = mkOption {
        type = nullOr str;
        default = null;
        description = "User name or numeric uid applied to both parent directories.";
      };

      group = mkOption {
        type = nullOr str;
        default = null;
        description = "Group name or numeric gid applied to both parent directories.";
      };

      mode = mkOption {
        type = nullOr (strMatching "[0-7]{3,4}");
        default = null;
        example = "0700";
        description = "Octal mode applied to both parent directories.";
      };
    };
  };

  entryType = submodule {
    options = {
      target = mkOption {
        type = str;
        description = "Path projected into the live filesystem.";
      };

      source = mkOption {
        type = nullOr str;
        default = null;
        description = "Relative path below the persistence store; derived from target when unset.";
      };

      kind = mkOption {
        type = enum ["directory" "file"];
        default = "directory";
        description = "Filesystem object created at source and target.";
      };

      method = mkOption {
        type = enum ["bind" "symlink"];
        default = "bind";
        description = "Whether to project the persistent object with a bind mount or symbolic link.";
      };

      manageMetadata = mkOption {
        type = bool;
        default = true;
        description = "Whether nixos-core applies ownership and mode. Disable this for filesystems without Unix metadata.";
      };

      owner = mkOption {
        type = nullOr str;
        default = null;
        description = "User name or numeric uid applied to the persistent object.";
      };

      group = mkOption {
        type = nullOr str;
        default = null;
        description = "Group name or numeric gid applied to the persistent object.";
      };

      mode = mkOption {
        type = nullOr (strMatching "[0-7]{3,4}");
        default = null;
        example = "0700";
        description = "Octal mode applied to the persistent object.";
      };

      parent = mkOption {
        type = parentType;
        default = {};
        description = "Metadata for the immediate source and target parent directories.";
      };
    };
  };

  entryType' = coercedTo str (target: {inherit target;}) entryType;
  directoryType = entryType';
  fileType =
    coercedTo str (target: {
      inherit target;
      kind = "file";
    })
    entryType;
  userStoreType = coercedTo (listOf entryType') (entries: {inherit entries;}) (submodule {
    options = {
      entries = mkOption {
        type = listOf entryType';
        default = [];
        description = "Legacy home-relative persistence entries.";
      };

      directories = mkOption {
        type = listOf directoryType;
        default = [];
        description = "Home-relative directories backed by this store.";
      };

      files = mkOption {
        type = listOf fileType;
        default = [];
        description = "Home-relative files backed by this store.";
      };
    };
  });

  storeType = submodule {
    options = {
      entries = mkOption {
        type = listOf entryType';
        default = [];
        description = "Legacy system persistence entries.";
      };

      directories = mkOption {
        type = listOf directoryType;
        default = [];
        example = ["/var/lib/nixos"];
        description = "System directories backed by this store.";
      };

      files = mkOption {
        type = listOf fileType;
        default = [];
        example = ["/etc/machine-id"];
        description = "System files backed by this store.";
      };

      users = mkOption {
        type = attrsOf userStoreType;
        default = {};
        example.alice = {
          directories = [".ssh" ".local/state"];
          files = [".config/example/state"];
        };
        description = "Home-relative files and directories backed by this store, grouped by NixOS user name.";
      };
    };
  };

  # TODO: check if Nixpkgs lib makes this any easier because this sucks
  isAbsolute = value: hasPrefix "/" value;
  normalizedRelative = path:
    !isAbsolute path
    && path != ""
    && !(hasInfix "/../" "/${path}/")
    && !(hasInfix "/./" "/${path}/");
  join = left: right: "${removeSuffix "/" left}/${removePrefix "/" right}";

  normalizeEntry = store: user: entry: let
    isUser = user != null;
    userConfig =
      if isUser
      then config.users.users.${user}
      else null;
    defaultOwner =
      if isUser
      then user
      else "root";
    defaultGroup =
      if isUser
      then userConfig.group
      else "root";
    target =
      if isUser
      then join userConfig.home entry.target
      else entry.target;
    sourceRelative =
      if entry.source != null
      then entry.source
      else removePrefix "/" target;
    metadataValue = explicit: fallback:
      if !entry.manageMetadata
      then null
      else if explicit != null
      then explicit
      else fallback;
    parentValue = explicit: fallback:
      if !entry.manageMetadata || !entry.parent.enable
      then null
      else if explicit != null
      then explicit
      else fallback;
  in {
    inherit store target;
    source = join store sourceRelative;
    inherit (entry) kind method;
    owner = metadataValue entry.owner defaultOwner;
    group = metadataValue entry.group defaultGroup;
    mode = metadataValue entry.mode (
      if entry.kind == "directory"
      then
        if user == null
        then "0755"
        else "0700"
      else "0600"
    );
    parentOwner = parentValue entry.parent.owner defaultOwner;
    parentGroup = parentValue entry.parent.group defaultGroup;
    parentMode = parentValue entry.parent.mode (
      if user == null
      then "0755"
      else "0700"
    );
  };

  normalizedEntries = concatLists (mapAttrsToList (
      store: storeConfig:
        map (normalizeEntry store null) storeConfig.entries
        ++ map (normalizeEntry store null) storeConfig.directories
        ++ map (normalizeEntry store null) storeConfig.files
        ++ concatLists (mapAttrsToList (
            user: userConfig:
              map (normalizeEntry store user) userConfig.entries
              ++ map (normalizeEntry store user) userConfig.directories
              ++ map (normalizeEntry store user) userConfig.files
          )
          storeConfig.users)
    )
    persistenceCfg.stores);
  stores = attrNames persistenceCfg.stores;
  # FIXME: figure out why writers.writeJSON crashed the service here
  plan = pkgs.writeText "nixos-core-persistence.json" (toJSON {
    version = 1;
    entries = normalizedEntries;
  });

  userNames = concatLists (mapAttrsToList (_: store: attrNames store.users) persistenceCfg.stores);
  declaredEntries = concatLists (mapAttrsToList (
      _: store:
        store.entries
        ++ store.directories
        ++ store.files
        ++ concatLists (mapAttrsToList (_: user: user.entries ++ user.directories ++ user.files) store.users)
    )
    persistenceCfg.stores);
  invalidSystemTargets = concatLists (mapAttrsToList (
      _: store:
        map (entry: entry.target) (filter (entry: !isAbsolute entry.target) store.entries)
    )
    persistenceCfg.stores);
  invalidUserTargets = concatLists (mapAttrsToList (
      _: store:
        concatLists (mapAttrsToList (
            _: user:
              map (entry: entry.target) (filter (entry: !normalizedRelative entry.target) (user.entries ++ user.directories ++ user.files))
          )
          store.users)
    )
    persistenceCfg.stores);
  invalidSources = map (entry: entry.source) (filter (
      entry: entry.source != null && !normalizedRelative entry.source
    )
    declaredEntries);

  pathContains = parent: path:
    parent
    == "/"
    || path == parent
    || hasPrefix "${removeSuffix "/" parent}/" path;
  mountPaths = unique (stores ++ map (entry: entry.target) normalizedEntries);

  pathsOverlap = left: right: pathContains left right || pathContains right left;
  overlappingTargets = map (entry: entry.target) (filter (
      entry: any (store: pathsOverlap entry.target store) stores
    )
    normalizedEntries);
in {
  options.system.nixos-core.persistence = {
    enable = mkEnableOption "filesystem-neutral persistent path projection";
    stores = mkOption {
      type = attrsOf storeType;
      default = {};
      example."/persist" = {
        entries = [
          "/var/lib/nixos"
          {
            target = "/etc/machine-id";
            kind = "file";
          }
        ];
        users.alice = [".ssh" ".local/state"];
      };
      description = ''
        Persistent stores and the paths projected from each store.

        A store may use any filesystem that can hold the configured objects
        and support the selected projection method. nixos-core does not
        create, format, snapshot, or reset it.
      '';
    };
  };

  config = mkIf (cfg.enable && persistenceCfg.enable) {
    system.build.nixosCorePersistencePlan = plan;

    # TODO: verify system dependencies
    systemd.services.nixos-core-persistence = {
      description = "Project nixos-core persistent paths";
      wantedBy = ["sysinit.target"];
      before = ["sysinit.target"];
      after = ["local-fs.target"];
      reloadTriggers = [plan];
      unitConfig = {
        DefaultDependencies = false;
        RequiresMountsFor = mountPaths;
      };

      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = ["${getExe' cfg.package "persist"} ${plan}"];
        ExecReload = ["${getExe' cfg.package "persist"} ${plan}"];
        ExecStop = ["${getExe' cfg.package "persist"} ${plan} --clear"];
      };
    };

    assertions = [
      {
        assertion = stores != [];
        message = "system.nixos-core.persistence requires at least one store";
      }
      {
        assertion = all (store: isAbsolute store && store != "/") stores;
        message = "nixos-core persistence store paths must be absolute and cannot be /";
      }
      {
        assertion = invalidSystemTargets == [];
        message = "nixos-core system persistence targets must be absolute: ${toJSON invalidSystemTargets}";
      }
      {
        assertion = invalidUserTargets == [];
        message = "nixos-core user persistence targets must be normalized paths relative to home: ${toJSON invalidUserTargets}";
      }
      {
        assertion = invalidSources == [];
        message = "nixos-core persistence sources must be normalized paths relative to their store: ${toJSON invalidSources}";
      }
      {
        assertion = overlappingTargets == [];
        message = "nixos-core persistence targets cannot contain, equal, or be contained by a store: ${toJSON overlappingTargets}";
      }
      {
        assertion = all (user: hasAttr user config.users.users) userNames;
        message = "Every nixos-core persistence user must exist in users.users";
      }
    ];
  };
}
