{
  description = "embr, the package manager built for Luau";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      eachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # The version and the description come from Cargo.toml, so a tag
      # needs no edit here.
      crate = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;
    in
    {
      packages = eachSystem (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = crate.name;
          inherit (crate) version;
          src = self;
          # Every dependency comes from crates.io and Cargo.lock holds its
          # sum, so no hash in this file changes from one release to the next.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ];
          # Some tests run git, as embr does.
          nativeCheckInputs = [ pkgs.git ];
          # The sandbox HOME is not writable, and the index tests write the
          # cache under it. No test needs the network: the HTTP tests answer
          # from a server on localhost.
          preCheck = ''
            export HOME=$(mktemp -d)
          '';
          # embr clones package indices and applies patches with git.
          postInstall = ''
            wrapProgram $out/bin/embr --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git ]}
          '';

          meta = {
            inherit (crate) description;
            homepage = "https://emberpm.dev";
            license = pkgs.lib.getLicenseFromSpdxId crate.license;
            mainProgram = "embr";
          };
        };
      });
    };
}
