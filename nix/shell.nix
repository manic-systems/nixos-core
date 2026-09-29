{
  mkShell,
  rustc,
  cargo,
  rustfmt,
  clippy,
  taplo,
  pkg-config,
  openssl,
  cargo-nextest,
}:
mkShell {
  name = "rust";

  strictDeps = true;
  nativeBuildInputs = [
    rustc
    cargo
    pkg-config

    # Tools
    (rustfmt.override {asNightly = true;})
    clippy
    taplo

    # Additional Cargo Tooling
    cargo-nextest
  ];
  buildInputs = [openssl];
}
