{
  description = "Development environment for Copperpour";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    ...
  }: let
    supportedSystems = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    pkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
  in {
    packages = forAllSystems (
      system: let
        pkgs = pkgsFor system;
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      in rec {
        copperpour = rustPlatform.buildRustPackage {
          pname = "copperpour";
          inherit version;

          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              pkgs.lib.cleanSourceFilter path type
              && !(type == "directory" && builtins.baseNameOf path == "target");
          };

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            cmake
            protobuf
            pkg-config
          ];

          PROTOC = "${pkgs.protobuf}/bin/protoc";
          PROTOC_INCLUDE = "${pkgs.protobuf}/include";

          cargoBuildFlags = [
            "-p"
            "copperpour"
            "--bin"
            "copperpour"
            # The name before the rename; runs copperpour.
            "--bin"
            "konnect"
          ];

          cargoTestFlags = [
            "-p"
            "copperpour"
            "--lib"
            "--tests"
          ];

          # Protocol tests spawn the packaged server. Give that child an
          # explicit writable state directory instead of Nix's read-only
          # sandbox home.
          preCheck = ''
            export COPPERPOUR_STATE_DIR="$TMPDIR/copperpour-state"
            mkdir -p "$COPPERPOUR_STATE_DIR"
          '';

          meta.mainProgram = "copperpour";
        };

        # The name before the rename.
        konnect = copperpour;
        default = copperpour;
      }
    );

    apps = forAllSystems (system: {
      copperpour = {
        type = "app";
        program = "${self.packages.${system}.copperpour}/bin/copperpour";
      };
      konnect = self.apps.${system}.copperpour;
      default = self.apps.${system}.copperpour;
    });

    devShells = forAllSystems (
      system: let
        pkgs = pkgsFor system;
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        python = pkgs.python3.withPackages (pythonPackages: [pythonPackages.jsonschema]);
        viewerLibraries = with pkgs; [
          dbus
          glib
          gtk3
          libappindicator-gtk3
          libayatana-appindicator
          librsvg
          libsoup_3
          openssl
          webkitgtk_4_1
        ];
      in {
        default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            protobuf
            cmake
            pkg-config
            file
            kicad-small
            patchelf
            python
            zip
          ];

          buildInputs = viewerLibraries;

          # copperpour-ipc/build.rs can discover these from PATH, but setting
          # them explicitly also makes protobuf's well-known types reliable
          # with Nix store paths.
          PROTOC = "${pkgs.protobuf}/bin/protoc";
          PROTOC_INCLUDE = "${pkgs.protobuf}/include";

          # The ignored real-KiCad integration test needs both kicad-cli and
          # the stock Device library, even on an otherwise clean machine.
          KICAD10_SYMBOL_DIR = "${pkgs.kicad.libraries.symbols}/share/kicad/symbols";

          # Cargo-built Tauri test binaries are not wrapped like installed
          # Nix applications, so expose their shared libraries while inside
          # the development shell.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath viewerLibraries;
        };
      }
    );
  };
}
