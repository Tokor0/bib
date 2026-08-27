{
  description = "bib — a bibliography manager with first-class Typst/hayagriva output";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;
        craneLib = crane.mkLib pkgs;

        # Required at runtime: pdftotext, pdfinfo, pdftoppm.
        runtimeInputs = [ pkgs.poppler-utils ];

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;

          # rustls is used instead of native-tls precisely so that neither
          # openssl nor pkg-config appears in the closure. Adding either here
          # would defeat the point of the project.
          buildInputs = [ ];
          nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        bib = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;

          # Put poppler on PATH by absolute store path so `bib` works even with
          # an empty PATH. `--prefix` (not `--suffix`) pins the tested version;
          # users who want a different poppler set `pdf.pdftotext` in
          # config.toml, which resolution consults before PATH.
          postInstall = ''
            wrapProgram $out/bin/bib \
              --prefix PATH : ${lib.makeBinPath runtimeInputs}
          '';

          meta = {
            description = "Bibliography manager with first-class Typst/hayagriva output";
            mainProgram = "bib";
            license = with lib.licenses; [ mit asl20 ];
            platforms = lib.platforms.unix;
          };
        });
      in
      {
        packages = {
          inherit bib;
          default = bib;
        };

        checks = {
          inherit bib;
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });
          fmt = craneLib.cargoFmt { inherit (commonArgs) src; };
          # The project exists because papis is hard to distribute, so the
          # properties that make `bib` easy to distribute are checks, not
          # aspirations. Each one has already been at risk once: adding rusqlite
          # pulled in a C SQLite, and a stray `buildInputs` entry is all it takes
          # to acquire OpenSSL.
          distribution = pkgs.runCommand "bib-distribution-check"
            {
              nativeBuildInputs = [ pkgs.binutils ];
            } ''
            unwrapped="${bib}/bin/.bib-wrapped"
            test -f "$unwrapped" || { echo "expected a wrapped binary"; exit 1; }

            echo "checking dynamic dependencies"
            libs=$(patchelf --print-needed "$unwrapped")
            echo "$libs"
            for lib in $libs; do
              case "$lib" in
                libc.so.*|libm.so.*|libgcc_s.so.*|ld-linux*) ;;
                *)
                  echo "unexpected dynamic dependency: $lib"
                  echo "the binary should need only libc, libm and libgcc;"
                  echo "a system SQLite or OpenSSL here defeats the point"
                  exit 1
                  ;;
              esac
            done

            echo "checking the runtime closure"
            for path in $(cat ${pkgs.writeClosure [ bib ]}); do
              case "$path" in
                *-python3*|*-python-3*)
                  echo "python in the runtime closure: $path"
                  exit 1
                  ;;
                *-tesseract-*|*-tessdata*)
                  echo "tesseract in the default closure: $path"
                  echo "OCR is optional and must not be in the default package"
                  exit 1
                  ;;
              esac
            done

            echo "checking the binary runs with no environment"
            env -i ${bib}/bin/bib --version > /dev/null

            touch $out
          '';

          test = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            # Turn a missing tool into a test failure rather than a silent skip.
            BIBTEST_REQUIRE_TYPST = "1";
            BIBTEST_REQUIRE_POPPLER = "1";
            # typst is a test-only dependency: `tests/typst.rs` compiles an
            # exported bibliography with the real binary.
            nativeBuildInputs =
              commonArgs.nativeBuildInputs ++ runtimeInputs ++ [ pkgs.typst ];
          });
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = runtimeInputs ++ [
            pkgs.rust-analyzer
            pkgs.cargo-nextest
            pkgs.typst
          ];
        };
      });
}
