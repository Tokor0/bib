# Home Manager module, exposed by the flake as `homeModules.bib`.
#
#   imports = [ inputs.bib.homeModules.bib ];
#   programs.bib = {
#     enable = true;
#     settings.libraries.main.dir = "~/Documents/library";
#   };
#
# The module is deliberately thin: `settings` is a freeform TOML attrset that
# mirrors `config.toml` key for key, so it cannot fall behind the Rust config
# model. The two places it does more than serialise — OCR wiring and build-time
# validation — are the two places where Nix knows something the TOML cannot say.

{ defaultPackage }:

{ config, lib, pkgs, ... }:

let
  cfg = config.programs.bib;
  format = pkgs.formats.toml { };

  # `pdf.tesseract` is an absolute store path rather than a name on PATH because
  # the package deliberately keeps tesseract out of its closure (see the
  # `distribution` check in flake.nix): OCR is opt-in, and this is how it is
  # opted into. `ocr_languages` comes from the same list that builds the
  # tessdata, since asking bib for a language tesseract was not built with is a
  # runtime failure with no obvious cause.
  ocrSettings = lib.optionalAttrs cfg.ocr.enable {
    pdf = {
      tesseract = lib.getExe cfg.ocr.package;
      ocr_languages = cfg.ocr.languages;
    };
  };

  settings = lib.recursiveUpdate cfg.settings ocrSettings;
  manageConfig = settings != { };

  generated = format.generate "bib-config.toml" settings;

  # `Config` is `deny_unknown_fields`, so a misspelled key does not quietly fall
  # back to a default — it fails every `bib` command. Running the real loader at
  # build time turns that into an error next to the offending Nix, which is most
  # of the reason to configure this declaratively at all.
  validated = pkgs.runCommand "bib-config.toml" { nativeBuildInputs = [ cfg.package ]; } ''
    if ! message=$(BIB_CONFIG=${generated} bib config get 2>&1 >/dev/null); then
      echo "programs.bib.settings is not a configuration bib accepts:" >&2
      echo "$message" >&2
      exit 1
    fi
    cp ${generated} $out
  '';

  # A cross-built configuration cannot run the loader; fall back rather than
  # refusing to evaluate.
  canValidate = pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform;

  configFile = if cfg.validateSettings && canValidate then validated else generated;

  # `bib` locates its user config with the `directories` crate, which is not XDG
  # on Darwin. Writing to `~/.config` there would leave every setting silently
  # unread.
  darwinConfigPath = "Library/Application Support/bib/config.toml";
in
{
  options.programs.bib = {
    enable = lib.mkEnableOption "bib, a bibliography manager with first-class Typst/hayagriva output";

    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPackage;
      defaultText = lib.literalExpression "the `bib` package built by this flake for the importing system";
      description = "The `bib` package to install.";
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = lib.literalExpression ''
        {
          default_library = "main";
          libraries.main.dir = "~/Documents/library";
          libraries.teaching.dir = "~/Documents/teaching";

          citekey = {
            max_length = 32;
            on_collision = "suffix-alpha";
          };
          folder.template = "{{ date.year }}/{{ citekey }}";
          providers.mailto = "you@example.com";
          providers.arxiv.rate_limit = "3s";
          export.exclude = [ "abstract" ];
          export.hayagriva.default_path = "bibliography.yml";
          open.pdf = "zathura {{ file }}";
        }
      '';
      description = ''
        Settings written to bib's user-level `config.toml`, mirroring that file
        key for key. Every key is checked when the configuration is built, so a
        typo is a build error rather than a `bib` that has stopped working.

        Leaving this empty leaves the file unmanaged, which is what you want if
        you would rather keep using `bib config set`: a store symlink is
        read-only, so the two ways of editing the file do not mix.

        Per-library settings (`<library>/.bib/config.toml`) and `BIB_*`
        environment variables still override what is set here.
      '';
    };

    validateSettings = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Check {option}`programs.bib.settings` by loading it with `bib` itself at
        build time. Turn this off to avoid building `bib` in order to build the
        configuration; the settings are then checked only when a `bib` command
        first runs.
      '';
    };

    ocr = {
      enable = lib.mkEnableOption ''
        OCR for scanned PDFs. This adds tesseract and its language data to the
        closure, which the `bib` package itself deliberately avoids
      '';

      languages = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "eng" ];
        example = [ "eng" "deu" "fra" ];
        description = ''
          Languages to make available to OCR. Used both to build the tessdata
          set and as `pdf.ocr_languages`, so the two cannot drift apart.

          Must include `"eng"`: tesseract's own data discovery fails without
          `eng.traineddata`, whatever else is present.
        '';
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.tesseract.override { enableLanguages = cfg.ocr.languages; };
        defaultText = lib.literalExpression
          "pkgs.tesseract.override { enableLanguages = config.programs.bib.ocr.languages; }";
        description = ''
          The tesseract package to use. Overriding it leaves
          {option}`programs.bib.ocr.languages` in charge of `pdf.ocr_languages`
          alone, so the package must carry the data for every language listed
          there.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    assertions = [
      {
        assertion = !cfg.ocr.enable || lib.elem "eng" cfg.ocr.languages;
        message = ''
          programs.bib.ocr.languages must include "eng": tesseract will not
          start without eng.traineddata in its tessdata directory.
        '';
      }
    ] ++ map
      (key: {
        assertion = !cfg.ocr.enable || !(cfg.settings ? pdf && cfg.settings.pdf ? ${key});
        message = ''
          programs.bib.settings.pdf.${key} conflicts with programs.bib.ocr,
          which writes that key itself. Set programs.bib.ocr.${
            if key == "tesseract" then "package" else "languages"
          } instead, or turn programs.bib.ocr.enable off.
        '';
      }) [ "tesseract" "ocr_languages" ];

    # Which file to write is a condition, not a choice of attribute path: the
    # module system needs to know the options a module defines before `pkgs` is
    # available to that module, so branching on `pkgs` for the path is an
    # infinite recursion.
    xdg.configFile = lib.mkIf (manageConfig && !pkgs.stdenv.hostPlatform.isDarwin) {
      "bib/config.toml".source = configFile;
    };

    home.file = lib.mkIf (manageConfig && pkgs.stdenv.hostPlatform.isDarwin) {
      ${darwinConfigPath}.source = configFile;
    };
  };
}
