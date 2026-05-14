with import <nixpkgs> {};
pkgs.mkShell {
  buildInputs = [
    cargo-udeps
    gdb # required for rust-gdb
    gh
    rustup
    rust-analyzer
    yamllint
  ];
}
