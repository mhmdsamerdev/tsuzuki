{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    # Rust toolchain manager — actual compiler version pinned via rustup below,
    # not via nixpkgs' own rustc, so `rustup show`/`rustup update` behave normally.
    pkgs.rustup

    # Linker Rust needs on Linux (`cc`). This is the whole reason we're not
    # fighting the host's dnf/gcc mismatch anymore.
    pkgs.gcc

    # Needed by any crate that links against a C library via *-sys crates
    # (pkg-config locates system libs; openssl backs the MAL HTTP client
    # in Phase 7 unless we end up on rustls instead).
    pkgs.pkg-config
    pkgs.openssl

    # Release builds vendor+statically-link their own OpenSSL (Cargo.toml's
    # `openssl = { features = ["vendored"] }") so distributed binaries don't
    # depend on the install machine's system libssl. Building that vendored
    # copy shells out to `perl ./Configure`, which needs a real perl with
    # its core modules (FindBin, etc.) on PATH — the host's own perl isn't
    # guaranteed to have these, so pull nixpkgs' own in ahead of it.
    pkgs.perl

    # Useful, not required: lets `cargo dist` (Phase 8) work without
    # reaching outside the shell for another toolchain.
    pkgs.cargo-dist

    # Mutation testing (CLAUDE.md's verification step).
    pkgs.cargo-mutants
  ];

  shellHook = ''
    # Ensure a stable toolchain + the components we rely on for testing/lint
    # are present the first time this shell is entered on a machine.
    rustup default stable >/dev/null 2>&1
    rustup component add clippy rustfmt >/dev/null 2>&1

    echo "tsuzuki dev shell ready — $(rustc --version)"
  '';
}
