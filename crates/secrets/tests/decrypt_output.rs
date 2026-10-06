use std::{fs, os::unix::fs::PermissionsExt};

use age::secrecy::ExposeSecret;

#[test]
fn decrypt_replaces_public_output_with_private_plaintext() {
  let dir = tempfile::tempdir().expect("temporary directory");
  let identity = age::x25519::Identity::generate();
  let key = dir.path().join("identity");
  let ciphertext = dir.path().join("token.age");
  let output = dir.path().join("token");

  fs::write(&key, identity.to_string().expose_secret())
    .expect("write identity");
  let encrypted =
    secrets::encrypt(b"private token", &[Box::new(identity.to_public())], false)
      .expect("encrypt token");
  fs::write(&ciphertext, encrypted).expect("write ciphertext");
  fs::write(&output, b"old token").expect("write prior output");
  fs::set_permissions(&output, fs::Permissions::from_mode(0o644))
    .expect("make prior output public");

  secrets::run(&[
    "secrets".into(),
    "decrypt".into(),
    "--identity".into(),
    key.display().to_string(),
    "--output".into(),
    output.display().to_string(),
    ciphertext.display().to_string(),
  ])
  .expect("decrypt into output file");

  assert_eq!(fs::read(&output).expect("read plaintext"), b"private token");
  assert_eq!(
    fs::metadata(&output)
      .expect("plaintext metadata")
      .permissions()
      .mode()
      & 0o777,
    0o600,
  );
}
