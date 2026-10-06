use std::{collections::BTreeMap, fs};

use age::{Identity, Recipient, secrecy::ExposeSecret};
use secrets::config::{Config, Host, Master, Secret};

fn roundtrip(armor: bool) {
  let id = age::x25519::Identity::generate();
  let recipient = id.to_public();

  let recips: Vec<Box<dyn Recipient + Send>> = vec![Box::new(recipient)];
  let ids: Vec<Box<dyn Identity>> = vec![Box::new(id)];

  let msg = b"correct horse battery staple\n";
  let ct = secrets::encrypt(msg, &recips, armor).expect("encrypt");
  assert_ne!(&ct[..], &msg[..], "ciphertext must differ from plaintext");
  let pt = secrets::decrypt(&ct, &ids).expect("decrypt");
  assert_eq!(
    &pt[..],
    &msg[..],
    "round-trip must preserve plaintext (armor={armor})"
  );
}

#[test]
fn x25519_roundtrip_binary() {
  roundtrip(false);
}

#[test]
fn x25519_roundtrip_armored() {
  roundtrip(true);
}

#[test]
fn canonical_rotation_removes_legacy_host_recipient() {
  let dir = tempfile::tempdir().expect("temp directory");
  let master_identity = age::x25519::Identity::generate();
  let host_identity = age::x25519::Identity::generate();
  let identity_file = dir.path().join("master-identity.txt");
  std::fs::write(&identity_file, master_identity.to_string().expose_secret())
    .expect("write master identity");

  let canonical = dir.path().join("canonical/service/example.age");
  let legacy_recipients: Vec<Box<dyn Recipient + Send>> = vec![
    Box::new(master_identity.to_public()),
    Box::new(host_identity.to_public()),
  ];
  secrets::secretio::encrypt_to_file(
    &canonical,
    b"secret",
    &legacy_recipients,
    false,
  )
  .expect("write legacy canonical ciphertext");

  let config = Config {
    version:    2,
    storage:    dir.path().to_path_buf(),
    master:     Master {
      recipients: vec![master_identity.to_public().to_string()],
      identities: vec![identity_file],
      plugin:     None,
    },
    hosts:      BTreeMap::from([("example".into(), Host {
      pubkeys: vec![host_identity.to_public().to_string()],
    })]),
    secrets:    BTreeMap::from([("service/example".into(), Secret {
      file:       "canonical/service/example.age".into(),
      host_files: BTreeMap::from([(
        "example".into(),
        "delivery/example/service/example.age".into(),
      )]),
      hosts:      vec!["example".into()],
      rekeyed:    true,
      generator:  None,
    })]),
    generators: BTreeMap::new(),
  };

  secrets::rekey::rekey_master(&config, dir.path(), None)
    .expect("rotate canonical ciphertext");

  let ciphertext = fs::read(canonical).expect("read rotated ciphertext");
  let master_identities: Vec<Box<dyn Identity>> =
    vec![Box::new(master_identity)];
  let plaintext =
    secrets::decrypt(&ciphertext, &master_identities).expect("master decrypts");
  assert_eq!(&*plaintext, b"secret");

  let host_identities: Vec<Box<dyn Identity>> = vec![Box::new(host_identity)];
  assert!(secrets::decrypt(&ciphertext, &host_identities).is_err());

  let count =
    secrets::rekey::rekey_all(&config, dir.path(), None).expect("rekey for host");
  assert_eq!(count, 1);
  let host_ciphertext =
    fs::read(dir.path().join("delivery/example/service/example.age"))
      .expect("read configured host ciphertext");
  let plaintext =
    secrets::decrypt(&host_ciphertext, &host_identities).expect("host decrypts");
  assert_eq!(&*plaintext, b"secret");
}

#[test]
fn master_rotation_keeps_unrekeyed_host_access() {
  let dir = tempfile::tempdir().expect("temp directory");
  let master = age::x25519::Identity::generate();
  let host = age::x25519::Identity::generate();
  let identity_file = dir.path().join("master-identity.txt");
  fs::write(&identity_file, master.to_string().expose_secret())
    .expect("write master identity");
  let canonical = dir.path().join("canonical/token.age");
  let recipients: Vec<Box<dyn Recipient + Send>> =
    vec![Box::new(master.to_public()), Box::new(host.to_public())];
  secrets::secretio::encrypt_to_file(
    &canonical,
    b"service token",
    &recipients,
    false,
  )
  .expect("write canonical ciphertext");

  let config = Config {
    version:    2,
    storage:    dir.path().to_path_buf(),
    master:     Master {
      recipients: vec![master.to_public().to_string()],
      identities: vec![identity_file],
      plugin:     None,
    },
    hosts:      BTreeMap::from([("example".into(), Host {
      pubkeys: vec![host.to_public().to_string()],
    })]),
    secrets:    BTreeMap::from([("token".into(), Secret {
      file:       "canonical/token.age".into(),
      host_files: BTreeMap::new(),
      hosts:      vec!["example".into()],
      rekeyed:    false,
      generator:  None,
    })]),
    generators: BTreeMap::new(),
  };
  secrets::rekey::rekey_master(&config, dir.path(), None)
    .expect("rotate canonical ciphertext");
  let ciphertext = fs::read(&canonical).expect("read rotated ciphertext");
  let identities: Vec<Box<dyn Identity>> = vec![Box::new(host)];
  assert_eq!(
    &*secrets::decrypt(&ciphertext, &identities)
      .expect("host decrypts rotated canonical"),
    b"service token",
  );
}
