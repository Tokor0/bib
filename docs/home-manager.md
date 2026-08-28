# Configuring `bib` with Home Manager

The flake exposes `homeModules.bib` (and `homeManagerModules.bib` under the
older output name). Add the flake as an input and import the module:

```nix
{
  inputs.bib.url = "github:you/bib";

  # in your home configuration
  imports = [ inputs.bib.homeModules.bib ];

  programs.bib = {
    enable = true;
    settings = {
      default_library = "main";
      libraries.main.dir = "~/Documents/library";
      libraries.teaching.dir = "~/Documents/teaching";

      citekey.max_length = 32;
      folder.template = "{{ date.year }}/{{ citekey }}";
      providers.mailto = "you@example.com";
      open.pdf = "zathura {{ file }}";
    };
  };
}
```

## `settings` is `config.toml`

There is no second schema to learn: `settings` is the TOML file, expressed as
Nix. Anything documented for `config.toml` goes in verbatim, and nothing about
the module needs updating when the Rust config model grows a key.

The file is written where `bib` actually looks for it, which is
`$XDG_CONFIG_HOME/bib/config.toml` on Linux and
`~/Library/Application Support/bib/config.toml` on macOS — `bib` finds its
config through the `directories` crate, which is not XDG on macOS.

Nothing else about precedence changes. A library's own
`<library>/.bib/config.toml` still overrides what is set here, and `BIB_*`
environment variables still override both.

## Settings are checked when you build, not when you run

`Config` is `deny_unknown_fields`, so a misspelled key is not ignored — it
fails *every* `bib` command. The module therefore loads the generated file with
`bib` itself at build time:

```
programs.bib.settings is not a configuration bib accepts:
error: invalid configuration (see /nix/store/…-bib-config.toml)
  caused by: unknown field: found `maxlength`, expected one of `templates`,
  `on_collision`, `max_length`, `normalize` for key "default.citekey.maxlength"
```

That is the point of configuring this declaratively rather than by hand. Set
`programs.bib.validateSettings = false` if you would rather not build `bib` in
order to build your home configuration.

## `bib config set` and `settings` do not mix

A managed file is a read-only symlink into the store, so `bib config set` and
`bib config edit` will fail against it. Pick one:

- leave `settings` empty (`programs.bib.enable = true` on its own installs the
  package and manages no file), and keep editing with `bib config set`; or
- put everything in `settings`, and treat the config file as generated.

## OCR

The `bib` package deliberately keeps tesseract out of its closure — OCR is
optional, and the `distribution` check fails if it ever leaks in. Opt in
through the module:

```nix
programs.bib.ocr = {
  enable = true;
  languages = [ "eng" "deu" ];
};
```

This builds a tesseract carrying exactly those languages, points
`pdf.tesseract` at it by absolute store path, and writes the same list to
`pdf.ocr_languages` — a language `bib` asks for that tesseract was not built
with is a runtime failure with no obvious cause, so the two are written from
one source. The list must include `"eng"`; tesseract will not start without
`eng.traineddata`, whatever else is present.

`pdf.ocr` is not part of this: set it in `settings` as usual, e.g.
`settings.pdf.ocr = "always"` to OCR every page rather than only those poppler
cannot read.
