//! Tests for the evaluated Secrets administration manifest.

use secrets::config::{Config, Manifest};

const BASE: &str = r#"
{
  "version": 2,
  "storage": "inventory/secrets",
  "master": { "recipients": ["age1master"], "identities": [], "plugin": null },
  "hosts": { "a": { "pubkeys": ["ssh-ed25519 KA"] } },
  "secrets": {
    "service/foo": {
      "file": "canonical/service/foo.age",
      "hostFiles": { "a": "delivery/a/service/foo.age" },
      "hosts": ["a"],
      "rekeyed": false,
      "generator": null
    }
  },
  "generators": {}
}
"#;

#[test]
fn evaluated_manifest_loads() {
  let cfg = Config::parse(BASE).unwrap();
  assert_eq!(cfg.secrets["service/foo"].hosts, ["a"]);
  assert_eq!(cfg.host_recipients("a").unwrap(), ["ssh-ed25519 KA"]);
}

#[test]
fn unsupported_version_is_rejected() {
  let err = Config::parse(&BASE.replace("\"version\": 2", "\"version\": 3"))
    .unwrap_err();
  assert!(format!("{err:#}").contains("version 3"));
}

#[test]
fn unknown_host_is_rejected() {
  let err =
    Config::parse(&BASE.replace("\"hosts\": [\"a\"]", "\"hosts\": [\"nope\"]"))
      .unwrap_err();
  assert!(format!("{err:#}").contains("nope"));
}

#[test]
fn secret_name_cannot_escape_storage() {
  let err =
    Config::parse(&BASE.replace("service/foo\"", "../outside\"")).unwrap_err();
  assert!(format!("{err:#}").contains("invalid secret name"));
}

#[test]
fn storage_must_be_relative() {
  let err =
    Config::parse(&BASE.replace("inventory/secrets", "/absolute/secrets"))
      .unwrap_err();
  assert!(format!("{err:#}").contains("storage path"));
}

#[test]
fn canonical_and_host_ciphertext_cannot_share_a_path() {
  let manifest = BASE
    .replace("delivery/a/service/foo.age", "canonical/service/foo.age")
    .replace("\"rekeyed\": false", "\"rekeyed\": true");
  let error =
    Config::parse(&manifest).expect_err("shared ciphertext must be rejected");
  assert!(format!("{error:#}").contains("ciphertext path"));
}

#[test]
fn unknown_rekey_host_is_not_a_successful_noop() {
  let cfg = Config::parse(BASE).expect("parse fixture");
  let storage = tempfile::tempdir().expect("temporary storage");
  assert_eq!(
    secrets::rekey::rekey_all(&cfg, storage.path(), Some("a"))
      .expect("host without rekeyed secrets"),
    0,
  );
  assert!(
    secrets::rekey::rekey_all(&cfg, storage.path(), Some("mistyped")).is_err()
  );
}

#[test]
fn runtime_manifest_rejects_nested_stage_and_output_paths() {
  let dir = tempfile::tempdir().expect("temporary manifest");
  let file = dir.path().join("manifest.json");
  let mut manifest = serde_json::json!({
    "store_dir": "/run/secrets",
    "secrets_dir": "/run/secrets.d",
    "templates_dir": "/run/secrets.t",
    "state_file": "/run/secrets-regular.manifest",
    "secrets": [
      {
        "name": "service",
        "file": "/ciphertext/one",
        "path": "/run/secrets.d/service",
        "owner": "root",
        "group": "root",
        "mode": "400",
        "placeholder": "{{SECRETS-one}}"
      },
      {
        "name": "service/token",
        "file": "/ciphertext/two",
        "path": "/run/secrets.d/token",
        "owner": "root",
        "group": "root",
        "mode": "400",
        "placeholder": "{{SECRETS-two}}"
      }
    ],
    "templates": []
  });
  std::fs::write(&file, manifest.to_string()).expect("write manifest");
  let error = Manifest::load(&file).expect_err("nested stage path must fail");
  assert!(format!("{error:#}").contains("contains another output"));

  manifest["secrets"][1]["name"] = "other".into();
  manifest["secrets"][1]["path"] = "/run/secrets.d/service/token".into();
  std::fs::write(&file, manifest.to_string()).expect("write manifest");
  let error = Manifest::load(&file).expect_err("nested public path must fail");
  assert!(format!("{error:#}").contains("contains another output"));
}
