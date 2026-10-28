{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    # Rust toolchain from nixpkgs.
    pkgs.cargo
    pkgs.rustc
    pkgs.clippy
    pkgs.rustfmt

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
    # Keep the registry cache in the project (gitignored), not in $HOME.
    export CARGO_HOME="$PWD/.cargo-home"

    echo "tsuzuki dev shell ready — $(rustc --version)"
  '';
}
