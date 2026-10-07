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
| `omaread BOOK [--chapter N] [--at TEXT]` | read one book, at a place |
| `omaread list [--json] [--filter TEXT]` | the library as a table |
| `omaread scan DIR [--filenames]` | add new books, notice moved ones |
| `omaread show BOOK [--json]` | what a book says about itself |
| `omaread set BOOK field=value …` | correct it, without touching the file |
| `omaread forget BOOK` | take a book (or a directory of them) out |
| `omaread find TEXT` | search the whole library and open the hit |
| `omaread export [DIR] [--force] [--reindex] [--embed]` | Markdown, one file per chapter |
| `omaread inspect BOOK` | spine and chapter sizes |
| `omaread images BOOK` | the pictures, and their own pixel sizes |
| `omaread blocks N BOOK` | the parsed blocks of one chapter |

`set` fields: `title`, `authors`, `series`, `series-index`, `tags`, `rating`,
`publisher`, `year`, `language`. An empty value clears the field. A correction
is a journal event: it wins over the file, survives a rescan, and never writes
into the EPUB.

`export` is for feeding a search engine; `--reindex` hands the result to
[qmd](https://github.com/tobi/qmd) and `--embed` also updates embeddings.
`find` uses qmd's index when it is installed, and searches the books directly
when it is not.

## Settings and data

Settings live in `~/.config/omaread/config.toml`, written with comments on
first start:

```toml
# Where the journal lives; point it at a synced folder to carry your
# reading position between machines. Each machine writes only its own file.
# journal_dir = "~/Dropbox/omaread/journal"

# Reading width in columns; leave out for the whole window.
# max_width = 66

# Picture backend: kitty, sixel or half-blocks; leave out to ask the terminal.
# images = "sixel"
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
