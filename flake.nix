# statusline-rs — the `statusline` UI extension for `rushi`, built as a
# Nix flake (Mode B, ext-flake-authoring.md).
#
# One crate, one output: a UI extension. `mkRushi` copies
# `external_ui_extensions` as `cp -rL "<src>/. "  "$out/ui_extensions/"`,
# so this package's `$out` must *contain* the per-name entry dir:
#   $out/statusline/ext.toml
#   $out/statusline/target/release/statusline-ext
# The entry dir name is `statusline` (the `ui_extensions/` entry); the
# binary name is `statusline-ext` (the crate's `[[bin]] name`). The
# ext.toml `command` is the relative `target/release/statusline-ext`,
# which the TUI resolves against the entry dir, so the release binary
# lands exactly where the resolver looks.
#
# See docs/reference/nix/ext-flake-authoring.md (skeleton §3, P1 P1 layout).

{
  description = "statusline-rs — statusline UI extension for rushi";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      pkgLib = nixpkgs.lib;

      buildFor = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = fenix.packages.${system}.stable.withComponents [
            "cargo" "clippy" "rust-src" "rustc" "rustfmt" "rust-analyzer"
          ];

          # Build one standalone cargo crate from a subpath of this flake's
          # source tree. For this repo the crate is at the flake root, so
          # `crateDir = ""`. The output binary name comes from the crate's
          # `[[bin]] name` in Cargo.toml (`statusline-ext`), not `crateName`.
          buildCrate = { crateDir, crateName }:
            pkgs.rustPlatform.buildRustPackage {
              pname = crateName;
              version = "0.1.0";
              src = if crateDir == "" then self else "${self}/${crateDir}";
              nativeBuildInputs = [ rustToolchain ];
              cargoLock = { lockFile = "${self}/Cargo.lock"; };
              doCheck = false;
            };

          # UI-ext wrapper: $out/<ext>/ext.toml + <ext>/<binDir>/<binary>.
          # `binDir` must equal the relative `command` path in ext.toml,
          # because the TUI resolves `command` against the ext entry dir.
          # buildRustPackage is a release build, so the default is
          # "target/release"; the binary is copied under its *binary* name
          # (statusline-ext), so the entry name and binary may differ.
          wrapAsExt = { extName, extToml, built, binDir ? "target/release" }:
            pkgs.stdenv.mkDerivation {
              name = "${extName}-ui-ext";
              nativeBuildInputs = [ built ];
              # No `src` to unpack: ext.toml and the binary are copied by
              # absolute path in installPhase. Skip the unpack check.
              unpackPhase = "true";
              installPhase = ''
                mkdir -p $out/${extName}/${binDir}
                cp ${extToml} $out/${extName}/ext.toml
                cp -rL ${built}/bin/. $out/${extName}/${binDir}/
              '';
            };
        in
        rec {
          # UI extension: entry dir `statusline`, binary `statusline-ext`,
          # release layout matching the ext.toml `command`.
          # Package key stays hyphen-free so `default` can reference it.
          statusline = wrapAsExt {
            extName = "statusline";
            extToml = "${self}/ext.toml";
            built = buildCrate { crateDir = ""; crateName = "statusline-ext"; };
          };

          default = statusline;
        };
    in
    {
      # Top-level `packages` (system as the inner key) is the standard
      # flake shape: `nix build .` resolves packages.<host>.default, and
      # the consumer reads extFlake.packages.<system>.statusline.
      packages = pkgLib.genAttrs supportedSystems (system: buildFor system);
    };
}
