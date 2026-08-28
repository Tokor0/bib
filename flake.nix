{
  description = "bib — a bibliography manager with first-class Typst/hayagriva output";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    # Used only by the `home-manager` check: the module is what ships, and a
    # module nothing ever evaluates is a module that is already broken.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs@{ flake-parts, crane, home-manager, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } ({ moduleWithSystem, ... }:
      let
        homeModules = rec {
          # `moduleWithSystem` resolves `packages.bib` against the system of
          # whatever configuration imports this, so the module needs neither
          # `self` nor a hand-written `packages.${system}` lookup — and it stays
          # a plain module function, importable without flake-parts.
          bib = moduleWithSystem (
            { config }:
            import ./nix/home-manager.nix { defaultPackage = config.packages.bib; }
          );
          default = bib;
        };
      in
      {
        # Listed rather than taken from a default set: nixpkgs has dropped
        # x86_64-darwin, and there is no point claiming a platform whose
        # nixpkgs throws before `bib` is even evaluated.
        systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];

        flake = {
          inherit homeModules;
          # The output name Home Manager used before it settled on
          # `homeModules`, kept so an older configuration needs no rename.
          homeManagerModules = homeModules;
        };

        perSystem = { config, pkgs, lib, ... }:
          let
            craneLib = crane.mkLib pkgs;

            # Required at runtime: pdftotext, pdfinfo, pdftoppm.
            runtimeInputs = [ pkgs.poppler-utils ];

            commonArgs = {
              src = craneLib.cleanCargoSource ./.;
              strictDeps = true;

              # rustls is used instead of native-tls precisely so that neither
              # openssl nor pkg-config appears in the closure. Adding either
              # here would defeat the point of the project.
              buildInputs = [ ];
              nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
            };

            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          in
          {
            packages.bib = craneLib.buildPackage (commonArgs // {
              inherit cargoArtifacts;

              # Put poppler on PATH by absolute store path so `bib` works even
              # with an empty PATH. `--prefix` (not `--suffix`) pins the tested
              # version; users who want a different poppler set `pdf.pdftotext`
              # in config.toml, which resolution consults before PATH.
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

            packages.default = config.packages.bib;

            checks = {
              inherit (config.packages) bib;

              clippy = craneLib.cargoClippy (commonArgs // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              });

              fmt = craneLib.cargoFmt { inherit (commonArgs) src; };

              # The project exists because papis is hard to distribute, so the
              # properties that make `bib` easy to distribute are checks, not
              # aspirations. Each one has already been at risk once: adding
              # rusqlite pulled in a C SQLite, and a stray `buildInputs` entry
              # is all it takes to acquire OpenSSL.
              distribution = pkgs.runCommand "bib-distribution-check"
                {
                  nativeBuildInputs = [ pkgs.binutils ];
                } ''
                unwrapped="${config.packages.bib}/bin/.bib-wrapped"
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
                for path in $(cat ${pkgs.writeClosure [ config.packages.bib ]}); do
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
                env -i ${config.packages.bib}/bin/bib --version > /dev/null

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

              # The Home Manager module is a shipped interface, so it is
              # evaluated here rather than trusted. It is imported the way a
              # user imports it — no `package` override — so the check also
              # covers `moduleWithSystem` resolving the default package.
              # Building the file it writes exercises the build-time settings
              # validation, which is the module's main promise over
              # hand-writing config.toml.
              home-manager-module =
                let
                  configuration = options: home-manager.lib.homeManagerConfiguration {
                    inherit pkgs;
                    modules = [
                      homeModules.bib
                      {
                        home = {
                          username = "reader";
                          homeDirectory = "/home/reader";
                          stateVersion = "25.05";
                        };
                        programs.bib = options // { enable = true; };
                      }
                    ];
                  };

                  configFile = home:
                    if pkgs.stdenv.hostPlatform.isDarwin
                    then home.config.home.file."Library/Application Support/bib/config.toml".source
                    else home.config.xdg.configFile."bib/config.toml".source;

                  plain = configFile (configuration {
                    settings = {
                      default_library = "main";
                      libraries.main.dir = "~/Documents/library";
                      citekey.max_length = 32;
                      providers.arxiv.rate_limit = "3s";
                    };
                  });

                  # Instantiated, not built: forcing the derivation is enough to
                  # catch a mistyped option or a tesseract that will not take
                  # `enableLanguages`, and stops short of compiling tesseract
                  # and its language data for the sake of a check.
                  ocr = configFile (configuration {
                    ocr = { enable = true; languages = [ "eng" "deu" ]; };
                  });
                in
                assert lib.isDerivation ocr;
                pkgs.runCommand "bib-home-manager-check" { } ''
                  cat ${plain}

                  echo "checking the settings reach the file bib reads"
                  grep -q 'max_length = 32' ${plain}
                  grep -q 'rate_limit = "3s"' ${plain}
                  grep -q 'dir = "~/Documents/library"' ${plain}

                  touch $out
                '';
            };

            devShells.default = craneLib.devShell {
              inherit (config) checks;
              packages = runtimeInputs ++ [
                pkgs.rust-analyzer
                pkgs.cargo-nextest
                pkgs.typst
              ];
            };
          };
      });
}
