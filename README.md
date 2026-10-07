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
| `omaread authors [--json]` | the distinct author names, and how many books each is on |
| `omaread scan DIR [--filenames]` | add the books of a directory to the library, and notice moved ones |
| `omaread show BOOK [--json]` | what a book says about itself |
| `omaread set BOOK field=value … [--json]` | correct one book, without touching the file |
| `omaread edit TEXT field=value … [--json]` | correct every book a word picks out, in one call |
| `omaread forget BOOK [--json]` | take a book, or a whole shelf, out of the library |
| `omaread journal status [--json]` | what the event log holds, and how much of it still matters |
| `omaread journal compact [--json]` | fold this machine's log to the events that still matter |
| `omaread find TEXT` | search the whole library and read the hit |
| `omaread export [DIR] [--force] [--reindex] [--embed]` | write the library as Markdown, one file per chapter |
| `omaread inspect BOOK` | what a book file holds, without the library |
| `omaread images BOOK` | the pictures in a book file, and how big each one is |
| `omaread blocks N BOOK` | the parsed blocks of one chapter of a book file |
| `omaread help [COMMAND]` | the same summary, or one command's own help |
| `omaread --version` | the version |

`set` fields: `title`, `authors`, `series`, `series-index`, `tags`, `rating`,
`publisher`, `year`, `language`. An empty value clears the field. `authors`
and `tags` take a comma-separated list, or a JSON array when a name holds a
comma of its own: `set BOOK 'authors=["Le Guin, Ursula"]'`. A correction is a
journal event: it wins over the file, survives a rescan, and never writes into
the EPUB.

### For scripts and agents

Each command is a verb on the library — `scan` adds, `set` and `edit` correct,
`forget` removes, `list`/`show`/`authors` read — and every one a program would
parse prints JSON with `--json`. A correction is always a journal event, so an
agent needs no private file format, only the command line.

The one to reach for when an agent has to tidy metadata is `edit`: it acts on
the whole set a word picks out, in one call. An author that a dozen files spell
a dozen ways is the case it exists for:

```bash
omaread authors                                   # the spellings, with counts
omaread list --filter "Murakami" --json           # preview what a word picks out
omaread edit "Haruki Murakami" 'authors=["村上春树"]' --json
```

`export` is for feeding a search engine; `--reindex` hands the result to
[qmd](https://github.com/tobi/qmd) and `--embed` also updates embeddings.
`find` uses qmd's index when it is installed, and searches the books directly
when it is not.

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
```

The journal is the source of truth: an append-only JSONL log, one line per
event, at `~/.local/share/omaread/journal/journal-<host>.jsonl`. Everything
else is folded out of it on start. Because a book is recognised by the hash of
its contents, moving or renaming it keeps its metadata and reading position.

### The log cleans up after itself

A log like this only grows, so it is folded now and then. Two things can no
longer affect the library, and the fold drops exactly those:

- everything before a book's last `book_forgotten` — the book was taken out and
  read in again, so its first life is dead;
- every reading position but the newest for a book, because a position is
  last-writer-wins.

Nothing else goes, so the library rebuilt from the folded log is the same
one — byte for byte. The fold runs by itself when a machine's own log passes
256 KB, and `omaread journal status` says how much of it still matters:

```bash
omaread journal status            # every file, and how much is dead weight
omaread journal compact           # fold this machine's log now
omaread journal status --json     # the same, for a program
```

Only this machine's own file is rewritten. A `journal-<otherhost>.jsonl` synced
in from elsewhere belongs to another machine that may be writing to it right
now, so it is read and left as it is; its own machine folds it. Because a fold
preserves the file's contribution to the whole, an old copy of a folded file
that a sync tool brings back still merges to the same library.

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
