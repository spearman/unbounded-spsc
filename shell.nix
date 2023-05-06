with import <nixpkgs> {};
pkgs.mkShell {
  buildInputs = [
    gdb # required for rust-gdb
    rustup
    rust-analyzer
  ];
}
