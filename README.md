# omaread

**English** · [中文](README.zh-CN.md)

A small terminal EPUB reader. One Rust binary — a library, reading positions
that survive moving a file or changing machine, pictures and MathML in the
text, and a command line a person or an agent can drive.

No service, no daemon, no database: your reading position is a plain,
append-only text log you can read, diff and sync.

## Install

Needs a Rust toolchain.

```bash
git clone https://github.com/77-223255/omaread
cd omaread
cargo build --release
install -m755 target/release/omaread ~/.local/bin/
```

The release builds on GitHub are statically linked (musl); a build of your own
links your system's libraries.

To open an EPUB by double-clicking it, also install the desktop entry, the
launcher and the icon:

```bash
install -Dm644 contrib/omaread.desktop ~/.local/share/applications/
install -Dm755 contrib/omaread-open ~/.local/bin/
install -Dm644 contrib/omaread.svg ~/.local/share/icons/hicolor/scalable/apps/
update-desktop-database -q ~/.local/share/applications
xdg-mime default omaread.desktop application/epub+zip
```

`omaread-open` asks `xdg-terminal-exec` for a terminal instead of guessing.

## Read

Add your books once, then open the library:

```bash
omaread scan ~/Books   # remember every EPUB below this directory
omaread                # open the library
```

Move with `j`/`k` and open a book with `Enter`; it continues where you stopped.
To read one file without adding it: `omaread ~/Downloads/book.epub`.

Press `?` for the full key list. The ones worth knowing first:

| Key | |
| --- | --- |
| `j` `k` | line down / up |
| `Space` `Backspace` | page down / up |
| `L` `H` (`]` `[`) | next / previous chapter |
| `t` `Tab` | table of contents |
| `/`, `n` `N` | search the book, next / previous hit |
| `i` | cursor in the text (links, movement) |
| `Enter`, `Ctrl-o` | follow a link, come back |
| `q` | library |
| `Q` | quit |

## Commands

`BOOK` is a book's id (or a unique prefix of at least 8 characters), a title or
author the library knows, or a file on disk.

| Command | |
| --- | --- |
| `omaread` | read the library |
| `omaread BOOK [--chapter N\|HREF] [--at TEXT]` | read one book, at a place |
| `omaread list [--json] [--filter TEXT]` | the library as a table |
| `omaread scan DIR [--filenames]` | add the books of a directory to the library, and notice moved ones |
| `omaread show BOOK [--json]` | what a book says about itself |
| `omaread set BOOK field=value …` | correct what a book says about itself, without touching the file |
| `omaread forget BOOK` | take a book, or a whole shelf, out of the library |
| `omaread find TEXT` | search the whole library and read the hit |
| `omaread export [DIR] [--force] [--reindex] [--embed]` | write the library as Markdown, one file per chapter |
| `omaread inspect BOOK` | what a book file holds, without the library |
| `omaread images BOOK` | the pictures in a book file, and how big each one is |
| `omaread blocks N BOOK` | the parsed blocks of one chapter of a book file |
| `omaread help [COMMAND]` | the same summary, or one command's own help |
| `omaread --version` | the version |

`set` fields: `title`, `authors`, `series`, `series-index`, `tags`, `rating`,
`publisher`, `year`, `language`. An empty value clears the field. A correction
is a journal event: it wins over the file, survives a rescan, and never writes
into the EPUB.

`export` is for feeding a search engine; `--reindex` hands the result to
[qmd](https://github.com/tobi/qmd) and `--embed` also updates embeddings.
`find` uses qmd's index when it is installed, and searches the books directly
when it is not.

## Sorting

In the library, `s` cycles the two plain orders — title, then author — and `S`
opens a box to sort by anything. Press `?` there for `j`, `k`, `/`, `s`, `S`
and the rest.

The author order and the super box ask a **decision model**: an LLM classifier
that answers typed questions, not a chat model. The author order sends the
author names and asks which part of each is the family name, so “Haruki
Murakami” and “Murakami, Haruki” land together. The super box sends every book
(title, authors, series, tags) and asks the model to score each one against
whatever you typed — “cozy for a rainy night”, “shortest first”, “the ones I
keep meaning to read” — then sorts by the score.

The model lives in [`src/decision/`](src/decision): a layer with no idea what a
book is, handed structured state and typed questions and handing back answers.
It speaks TypeSafe’s “System One” protocol (`typesafe/jev-1.13`) through
OpenRouter. This reader’s own two questions are in [`src/sorts.rs`](src/sorts.rs);
the layer is written to be lifted into another program as it stands. A sort
that means the same thing twice is answered from the session’s cache. With no
key the reader still works: author falls back to the plain alphabetical order
and the super box says so.

Give it an OpenRouter key in `OPENROUTER_API_KEY` or
`OMAREAD_DECISION_API_KEY`, or point at another endpoint in `[decision]` below.
On a machine that already runs [pi](https://pi.dev), the OpenRouter key pi
stores is used when no other is set — a convenience for trying it out, not a
dependency.

## Settings and data

Settings live in `~/.config/omaread/config.toml`, written with comments on
first start:

```toml
# omaread settings

# Where the journal lives. It is the source of truth for reading
# positions. Point this at a synchronised folder to
# carry your reading position between machines. Each machine writes
# only its own file, so no conflict can arise.
# journal_dir = "~/.local/share/omaread/journal"

# Reading width in columns. Comment out to use the full window.
# max_width = 66

# How pictures are drawn. Left out, the terminal is asked and the
# best of kitty, sixel and half-blocks is used. Inside tmux only
# half-blocks work, because tmux manages the screen itself.
# images = "sixel"

# The decision model a super sort asks. Left out, sorts fall back
# to the plain order. The key comes from OMAREAD_DECISION_API_KEY
# or the variable named below, and is never written here.
# [decision]
# base_url = "https://openrouter.ai/api/v1"
# model = "typesafe/jev-1.13"
# api_key_env = "OPENROUTER_API_KEY"
```

The journal is the source of truth: an append-only JSONL log, one line per
event, at `~/.local/share/omaread/journal/journal-<host>.jsonl`. Everything
else is folded out of it on start. Because a book is recognised by the hash of
its contents, moving or renaming it keeps its metadata and reading position.

## Notes

- **Pictures.** A cover and a pre-paginated page fill the room; a picture in
  the body is magnified up to four times to fill the room; a mark (a small,
  glyph-sized image) keeps its own size. Pictures are centred, capped at four
  times their own size in cells, and drawn with kitty or sixel where the
  terminal supports them, half-blocks everywhere else (and always inside tmux).
- **Formulas.** MathML is set on one line, with real Unicode super- and
  subscripts where they exist and plain `_x` / `^(x)` where they do not.
- **Theme.** Colours follow the active Omarchy theme when one is installed,
  and fall back to built-in colours otherwise.
- **Languages.** The interface follows `LC_ALL` / `LC_MESSAGES` / `LANG`;
  machine-readable output (`--json`, tables) stays English.

## Not there yet

PDF, MOBI and the other Kindle formats; reading two pages side by side.

## License

MIT. See [LICENSE](LICENSE).

## Credits

omaread started as a fork of [**omalibre**](https://github.com/AlexZeitler/omalibre)
by Alexander Zeitler, and has since been rewritten for personal use — the
library, the journal and the reader are its own now. The original MIT licence
and copyright are kept in [LICENSE](LICENSE).
