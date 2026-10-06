use age::secrecy::ExposeSecret;

#[test]
// XXX: shame on me.
fn missing_identity_file_does_not_prevent_decryption_with_another() {
  let dir = tempfile::tempdir().expect("temp directory");
  let missing = dir.path().join("missing-identity");
  let present = dir.path().join("present-identity");
  let identity = age::x25519::Identity::generate();
  std::fs::write(
    &present,
    format!("{}\n", identity.to_string().expose_secret()),
  )
  .expect("write identity");

  let identities =
    secrets::load_identities(&[missing, present], None).expect("load identities");
  let ciphertext =
    secrets::encrypt(b"secret", &[Box::new(identity.to_public())], false)
      .expect("encrypt");
  assert_eq!(
    &*secrets::decrypt(&ciphertext, &identities).expect("decrypt"),
    b"secret"
  );
}

#[test]
fn no_usable_identity_has_an_actionable_error() {
  let identities = Vec::new();
  let err = secrets::decrypt(b"", &identities)
    .expect_err("missing identities must not decrypt");
  assert!(format!("{err:#}").contains("no usable identities supplied"));
}
