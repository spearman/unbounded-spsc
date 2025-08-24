with import <nixpkgs> {};
pkgs.mkShell {
  buildInputs = [
    cargo-udeps
    gdb # required for rust-gdb
    rustup
    rust-analyzer
    yamllint
  ];
}
