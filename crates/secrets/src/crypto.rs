//! Core age encryption/decryption over the age v1 file format.
//!
//! Output is byte-compatible with `age`/`rage`: binary by default, or ASCII
//! armor when requested. Decryption transparently accepts either framing.

use std::io::{Read, Write};

use age::{Identity, Recipient};
use anyhow::{Context, Result, bail};
use zeroize::Zeroizing;

/// Encrypt `plaintext` to `recipients`. When `armor` is set the output is
/// PEM-style ASCII armor; otherwise it is the raw binary age format.
pub fn encrypt(
  plaintext: &[u8],
  recipients: &[Box<dyn Recipient + Send>],
  armor: bool,
) -> Result<Vec<u8>> {
  if recipients.is_empty() {
    bail!("refusing to encrypt with no recipients");
  }

  let encryptor = age::Encryptor::with_recipients(
    recipients.iter().map(|r| r.as_ref() as &dyn Recipient),
  )
  .context("building age encryptor")?;

  let format = if armor {
    age::armor::Format::AsciiArmor
  } else {
    age::armor::Format::Binary
  };

  let mut out = Vec::new();
  let armored = age::armor::ArmoredWriter::wrap_output(&mut out, format)
    .context("initializing armor writer")?;
  let mut writer = encryptor
    .wrap_output(armored)
    .context("wrapping age output")?;
  writer.write_all(plaintext).context("writing plaintext")?;
  let armored = writer.finish().context("finalizing age stream")?;
  armored.finish().context("finalizing armor")?;
  Ok(out)
}

/// Decrypt an age file (binary or armored) using the first matching identity.
///
/// The plaintext is returned in a [`Zeroizing`] buffer so it is scrubbed from
/// memory when dropped.
pub fn decrypt(
  ciphertext: &[u8],
  identities: &[Box<dyn Identity>],
) -> Result<Zeroizing<Vec<u8>>> {
  if identities.is_empty() {
    bail!(
      "no usable identities supplied; pass an existing --identity or --plugin"
    );
  }

  let reader = age::armor::ArmoredReader::new(ciphertext);
  let decryptor = age::Decryptor::new(reader).context("reading age header")?;

  let mut stream = decryptor
    .decrypt(identities.iter().map(|i| i.as_ref() as &dyn Identity))
    .context("no identity could decrypt this file")?;

  let mut out = Zeroizing::new(Vec::new());
  stream
    .read_to_end(&mut out)
    .context("reading decrypted payload")?;
  Ok(out)
}
