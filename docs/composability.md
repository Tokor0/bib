# Driving `bib` from other tools

`bib` is built to be a good citizen in a pipeline and behind a launcher. The
contract below is stable and tested (`tests/composability.rs`); the Noctalia
plugin is a separate package, and this is what it is written against.

## The rules

1. **stdout carries the payload, stderr carries everything else.** With
   `--json`, `--format` or `--keys`, stdout is *only* the results. Progress,
   provider notes and warnings always go to stderr, including from `bib add`
   and `bib update`.
2. **Actions key off an opaque `id`, never a list position.** Every result
   carries a self-contained handle — `doi:…`, `arxiv:…`, `isbn:…`, or a cite
   key. By the time a launcher activates a row, the result set may have been
   re-queried; a position would be meaningless.
3. **Zero results is success.** `--json` prints `[]` and exits 0. Non-zero means
   a real failure, so "nothing matched" and "misconfigured" are distinguishable
   without parsing anything.
4. **`bib search` never touches the network; `bib find` always does.** That is
   why they are separate commands rather than one with a flag.

## The result schema

`bib search --json` (local) and `bib find --json` (web) emit the *same* objects,
so a caller needs one field mapping for both:

```json
{
  "id": "doi:10.1002/andp.19053221004",
  "source": "library",
  "citekey": "einstein1905zur",
  "cite": "@einstein1905zur",
  "title": "Zur Elektrodynamik bewegter Körper",
  "subtitle": "Einstein · 1905 · Annalen der Physik",
  "year": 1905,
  "authors": ["Einstein"],
  "container": "Annalen der Physik",
  "tags": ["relativity"],
  "files": ["/home/you/library/einstein1905zur/paper.pdf"],
  "in_library": true
}
```

`source` is `library` or the provider name. `cite` and `files` are present so
the common activations need **no second process**: the clipboard actions and
opening the document can be done from the row alone.

## Noctalia

Two calls, merged: `bib search` answers in milliseconds from the local index,
`bib find` follows when the network does.

```lua
-- plugin.toml:  prefix = "bib", debounce = 300
function onQuery(query)
  noctalia.runAsync({ "bib", "search", query, "--json" }, function(res)
    launcher.setResults(query, rows(res.stdout))
  end, 2000)

  noctalia.runAsync({ "bib", "find", query, "--json", "--timeout", "5s" }, function(res)
    launcher.setResults(query, rows(res.stdout))
  end, 8000)
end

function rows(stdout)
  local parsed = noctalia.json.decode(stdout) or {}
  local out = {}
  for _, r in ipairs(parsed) do
    table.insert(out, {
      id       = r.id,
      title    = r.title,
      subtitle = r.subtitle,
      badge    = r.in_library and "library" or r.source,
      glyph    = "article",
    })
  end
  return out
end
```

Use the **argv table form** of `runAsync`, not a string: the query is whatever
the user typed, and the argv form passes it as one argument with no shell
involved. `timeoutMs` is clamped to `[50, 60000]`, so keep `--timeout` below it.

Activation, given the row's `id`:

| Action | How |
|---|---|
| Copy `@citekey` | already in the row as `cite` — `noctalia.copyToClipboard(cite)` |
| Copy the identifier | already in the row as `id` |
| Open the document | `runAsync({"bib", "open", citekey})` — honours `[open].pdf` |
| Add a web result | `runAsync({"bib", "add", id, "--fetch"})` |

## wofi / dmenu / fzf

`--format` is a minijinja template with the same variables and filters as
cite-key templates, and renders one line per result with newlines collapsed —
so one line is always one item.

```sh
# Pick a document and open it.
bib list --format '{{ id }}\t{{ title }} — {{ subtitle }}' \
  | wofi --dmenu --prompt bib \
  | cut -f1 \
  | xargs -r -I{} bib open {}

# Copy a Typst citation.
bib list --format '{{ cite }}\t{{ title }}' \
  | wofi --dmenu | cut -f1 | wl-copy

# Search the web and file what you pick.
bib find "attention is all you need" --format '{{ id }}\t{{ title }}' \
  | wofi --dmenu | cut -f1 | xargs -r -I{} bib add {} --fetch
```

`--keys` is the shortest form when only cite keys are wanted:

```sh
bib search 'tag:relativity' --keys | xargs bib export -o relativity.yml

# Every command that takes cite keys takes a list of them, so this is also how
# a whole library is acted on — downloading the documents it is missing, say.
bib list --keys | xargs bib fetch
```

`bib fetch` leaves `fetch.rate_limit` between requests to the same host, so a
run like that stays inside what arXiv asks of a client.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, including zero results |
| 1 | A real failure: bad query, unreadable library, no provider could resolve |

## Latency

Measured on a 2000-document library: `bib search --json` takes **~10 ms** warm,
against a typical launcher debounce of 300 ms. The first run after a change
rebuilds the index (~270 ms for 2000 documents) and subsequent runs only stat
the files. There is deliberately no daemon: a process per keystroke is cheap
enough, and a long-lived one would add state, a socket and a lifecycle to a tool
whose premise is a single static binary.

`bib find` is bounded by `--timeout` (default 8 s) and returns whatever arrived,
naming the providers it gave up on via stderr.
