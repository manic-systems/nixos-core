//! secrets: an age-format-compatible secrets manager.
//!
//! The library exposes the primitives the CLI is built from: recipient/identity
//! parsing (age, OpenSSH, and plugin keys such as YubiKey), age v1
//! encryption/decryption, the admin workflows (edit, rekey, generate, verify)
//! driven by an evaluated Nix inventory, and the runtime activator that
//! installs a host's secrets from a NixOS-generated manifest.

pub mod activate;
pub mod admin;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod editor;
pub mod generate;
pub mod identity;
pub mod rekey;
pub mod secretio;
pub mod tools;
pub mod verify;

pub use cli::run;
pub use config::{Config, Manifest};
pub use crypto::{decrypt, encrypt};
pub use identity::{
  build_recipients,
  load_identities,
  parse_recipient,
  read_recipients_file,
};
