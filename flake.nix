{
  description = "Tauri Dev Env";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        tauriDeps = with pkgs; [
          webkitgtk_4_1
          gtk3
          libsoup_3
          glib
          cairo
          pango
          gdk-pixbuf
          atk
          openssl
          librsvg
          libappindicator-gtk3
          dbus
        ];
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust
            nodejs_20
            pkg-config
            gobject-introspection
            curl
          ] ++ tauriDeps;

          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath tauriDeps}:$LD_LIBRARY_PATH"
            export GIO_MODULE_DIR="${pkgs.glib-networking}/lib/gio/modules/"
            export XDG_DATA_DIRS="${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}:$XDG_DATA_DIRS"
            echo "Dev environment ready!"
            echo "Run: npm install && npm tauri dev"
          '';
        };
      }
    );
}
