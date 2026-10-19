{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    # Toolchain manager; the compiler comes from rustup, not nixpkgs.
    pkgs.rustup

    # Linker (`cc`) for Rust, and the C/asm part of ring.
    pkgs.gcc

    # Release packaging (`cargo dist`).
    pkgs.cargo-dist

    # Mutation testing.
    pkgs.cargo-mutants

    # Test runner with one summary line (`cargo nextest run`).
    pkgs.cargo-nextest
  ];

  shellHook = ''
    # Install the stable toolchain plus clippy and rustfmt on first entry.
    rustup default stable >/dev/null 2>&1
    rustup component add clippy rustfmt >/dev/null 2>&1

    echo "tsuzuki dev shell ready — $(rustc --version)"
  '';
}
