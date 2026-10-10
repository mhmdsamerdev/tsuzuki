{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    # Toolchain manager; the compiler comes from rustup, not nixpkgs.
    pkgs.rustup

    # Linker (`cc`) for Rust.
    pkgs.gcc

    # Locate and provide the OpenSSL that native-tls builds against.
    pkgs.pkg-config
    pkgs.openssl

    # The vendored OpenSSL build runs `perl ./Configure`, which needs perl's core modules.
    pkgs.perl

    # Release packaging (`cargo dist`).
    pkgs.cargo-dist

    # Mutation testing.
    pkgs.cargo-mutants
  ];

  shellHook = ''
    # Install the stable toolchain plus clippy and rustfmt on first entry.
    rustup default stable >/dev/null 2>&1
    rustup component add clippy rustfmt >/dev/null 2>&1

    echo "tsuzuki dev shell ready — $(rustc --version)"
  '';
}
