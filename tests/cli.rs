//! End-to-end checks of the command line.
//!
//! The unit tests inside the binary check the pieces; these check the wiring a
//! unit test cannot: commands running against a real library, the reader
//! drawing on a real terminal, and the environment deciding where the files
//! go. Every run gets its own data and config directory, so a test never reads
//! or writes the library this machine actually keeps.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// The fixture kit the unit tests share, pulled in by path: this is a separate
// crate, but a book written here must be the same book the reader's own tests
// read.
#[path = "../src/testkit.rs"]
mod testkit;

use testkit::Scratch;

/// The scratch installation's environment: every XDG variable names a
/// directory of its own, so a run reads and writes nothing this machine
/// actually keeps.
fn scratch_env(command: &mut Command, scratch: &Path) {
    command
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_STATE_HOME", scratch.join("state"))
        // English, whatever this machine reads: these tests are about words.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .current_dir(scratch);
}

/// One command, run against a scratch installation.
fn omaread(scratch: &Path) -> Command {
    std::fs::create_dir_all(scratch).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_omaread"));
    scratch_env(&mut command, scratch);
    command
}

/// A scratch directory for one test, emptied first so each test starts from
/// no library: unique to this run, so two `cargo test` runs cannot delete each
/// other's fixtures, and removed when the test ends, however it ends.
fn scratch(name: &str) -> Scratch {
    let dir = Scratch::new(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The value shown on the line that starts with `label`, whatever the padding
/// between the two. The field/value pair is what a test is about; the column it
/// sits in is not.
fn field(text: &str, label: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix(label))
        .map(|value| value.trim().to_string())
}

/// A minimal EPUB with one chapter per entry, which is all any of these
/// commands asks of a book. The author sits in the file, because reading one
/// back out again is what `authors` is about.
fn book(path: &Path, title: &str, chapters: &[&str]) {
    let bodies: Vec<String> = chapters
        .iter()
        .enumerate()
        .map(|(i, text)| format!("<h1>Chapter {n}</h1><p>{text}</p>", n = i + 1))
        .collect();
    let bodies: Vec<&str> = bodies.iter().map(String::as_str).collect();
    place(
        path,
        testkit::named_book(
            "cli-book",
            title,
            "Stephenson",
            &format!("urn:test-{title}"),
            &bodies,
        ),
    );
}

/// The book where the test asked for it: the shared writer builds in the temp
/// dir, and the library scans a directory of its own.
fn place(path: &Path, built: PathBuf) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(built, path).unwrap();
}

/// A scratch library with one two-chapter book in it.
fn scanned(name: &str) -> Scratch {
    let dir = scratch(name);
    let file = dir.join("books/anathem.epub");
    book(
        &file,
        "Anathem",
        &["first chapter text", "second chapter text"],
    );
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    dir
}

/// A scratch library with one scanned book, and the id the library gave it:
/// the fixture the library commands are answered against.
fn a_scanned_library(name: &str) -> (Scratch, String) {
    let dir = scanned(name);
    let id = testkit::id_of(&dir.join("books/anathem.epub"));
    (dir, id)
}

/// Where the reader stopped, written into the library's journal the way the
/// reader writes it.
fn a_recorded_position(dir: &Path, id: &str) {
    let journal_dir = dir.join("data/omaread/journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    std::fs::write(
        journal_dir.join("journal-test.jsonl"),
        format!(
            "{{\"at\":\"2020-01-01T00:00:00Z\",\"book\":\"{id}\",\
             \"type\":\"position_set\",\"href\":\"OEBPS/ch1.xhtml\",\"block\":2,\"offset\":1}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn an_empty_shelf_points_at_the_scan_that_fills_it() {
    // Nothing has been scanned: the empty shelf points at the scan that fills
    // it rather than at a flag the command no longer has.
    let dir = scratch("empty-shelf");
    let out = omaread(&dir).arg("list").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("omaread scan"), "{}", stdout(&out));
}

#[test]
fn one_file_scanned_is_one_book_counted_in_the_singular() {
    let (dir, _id) = a_scanned_library("one-book");
    let out = omaread(&dir).arg("list").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("1 book\n"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("1 books"), "{}", stdout(&out));
}

#[test]
fn show_answers_from_the_file_before_anything_is_set_by_hand() {
    let (dir, _id) = a_scanned_library("show-by-path");
    let file = dir.join("books/anathem.epub");
    let out = omaread(&dir).arg("show").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("Anathem"), "{}", stdout(&out));
}

#[test]
fn a_set_correction_is_shown_by_name_by_id_prefix_and_in_the_json_list() {
    let (dir, id) = a_scanned_library("set-show");
    let out = omaread(&dir)
        .args([
            "set",
            "Anathem",
            "title=Other",
            "series=Norfolk",
            "rating=5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    // A correction the file cannot make, which `show` then answers — by the
    // new name and by a prefix of the id.
    for reference in ["Other", &id[..12]] {
        let out = omaread(&dir).arg("show").arg(reference).output().unwrap();
        assert!(out.status.success(), "{reference}: {}", stderr(&out));
        assert!(stdout(&out).contains("Other"), "{reference}: {out:?}");
    }
    // The record the whole library is listed from carries the same answer.
    let out = omaread(&dir).args(["list", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let books: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(books[0]["title"], "Other");
    assert_eq!(books[0]["series"], "Norfolk");
    assert_eq!(books[0]["rating"], 5);
    assert!(books[0]["files"][0]
        .as_str()
        .unwrap()
        .ends_with("anathem.epub"));
}

#[test]
fn the_reading_position_travels_with_the_record() {
    let (dir, id) = a_scanned_library("position");
    a_recorded_position(&dir, &id);
    let out = omaread(&dir).args(["list", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let books: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(books[0]["position"]["href"], "OEBPS/ch1.xhtml");
    assert_eq!(books[0]["position"]["block"], 2);
}

#[test]
fn forget_takes_the_book_and_the_scan_brings_back_nothing() {
    let (dir, id) = a_scanned_library("forget");
    a_recorded_position(&dir, &id);
    let out = omaread(&dir).args(["forget", "Anathem"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = omaread(&dir).args(["list", "--json"]).output().unwrap();
    let books: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(books.as_array().map(Vec::len), Some(1), "one book back");
    assert_eq!(books[0]["title"], "Anathem", "and none of the corrections");
    assert!(
        books[0]["position"].is_null(),
        "the position was not taken with the book: {books}"
    );
}

#[test]
fn the_reader_opens_a_book_and_quits() {
    // The reader needs a terminal and a test has none to give, so `script`
    // hands the binary a pty. The book is named on the command line, the
    // reader draws it, and `q` leaves — the whole way through, from an
    // argument to a restored terminal and an exit that says nothing went wrong.
    let dir = scratch("reader");
    let file = dir.join("one.epub");
    book(&file, "One", &["first chapter text", "second chapter text"]);

    let binary = format!("'{}'", env!("CARGO_BIN_EXE_omaread"));
    let mut terminal = Command::new("script");
    terminal.args(["-qec", &format!("{binary} '{}'", file.display()), "/dev/null"]);
    scratch_env(&mut terminal, &dir);
    let mut child = terminal.stdin(std::process::Stdio::piped()).spawn().unwrap();
    // The book opens as soon as the reader has drawn it, so a short wait
    // and then the key that leaves.
    std::thread::sleep(std::time::Duration::from_millis(500));
    child.stdin.take().unwrap().write_all(b"q").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // And the position it opened at was written down: reopening without
    // moving adds nothing, but the journal exists now.
    let journal = dir.join("data/omaread/journal");
    assert!(
        std::fs::read_dir(&journal)
            .is_ok_and(|entries| entries.count() >= 1),
        "the reader wrote no journal under {}",
        journal.display()
    );
}

#[test]
fn blocks_reports_a_chapter_and_refuses_a_number_out_of_range() {
    let dir = scratch("blocks");
    let file = dir.join("one.epub");
    book(&file, "One", &["first chapter text", "second chapter text"]);
    let file = file.to_str().unwrap().to_string();

    // Zero used to mean the first chapter, and a number past the end was
    // clamped to something that exists: both are refused, with a failure
    // status rather than the wrong chapter.
    for bad in ["0", "3"] {
        let out = omaread(&dir).args(["blocks", bad, &file]).output().unwrap();
        assert!(!out.status.success(), "blocks {bad} must be refused");
    }

    let out = omaread(&dir).args(["blocks", "2", &file]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ch1.xhtml"), "{}", stdout(&out));

    // And a book the library knows is answered the same way as a path.
    let dir = scratch("blocks-library");
    let file = dir.join("books/anathem.epub");
    book(&file, "Anathem", &["first chapter text"]);
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = omaread(&dir).args(["blocks", "1", "Anathem"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ch0.xhtml"), "{}", stdout(&out));
}

#[test]
fn inspect_reports_what_it_found_for_pictures() {
    // What `inspect` shows for the two things that classify a picture: the
    // cover it found and the layout the publication declared. A person whose
    // cover draws small needs to see whether the book ever named one.
    let dir = scratch("inspect-pictures");
    let file = dir.join("fixed.epub");
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:rendition="http://www.idpf.org/2007/ops"
         version="3.0" unique-identifier="id" rendition:layout="pre-paginated">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Fixed</dc:title><dc:language>en</dc:language>
<dc:identifier id="id">urn:fixed</dc:identifier>
</metadata>
<manifest>
<item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
<item id="cv" href="cover.png" media-type="image/png" properties="cover-image"/>
</manifest>
<spine><itemref idref="c0"/></spine></package>"#;
    place(
        &file,
        testkit::book_with(
            "inspect-fixed",
            "Fixed",
            &[r#"<p>a panel</p>"#],
            &[
                ("mimetype", b"application/epub+zip"),
                ("OEBPS/content.opf", opf.as_bytes()),
            ],
        ),
    );

    let out = omaread(&dir).arg("inspect").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(field(&text, "layout:").as_deref(), Some("pre-paginated"));
    // The cover's path is resolved against the package document, like any
    // other href in it.
    assert_eq!(field(&text, "cover:").as_deref(), Some("OEBPS/cover.png"));

    // A book that declared nothing reads `-`, like any other absent value:
    // reflowable is the default, and printing it as a declaration would
    // claim more than the book said.
    let plain = dir.join("plain.epub");
    book(&plain, "Plain", &["first chapter text"]);
    let out = omaread(&dir).arg("inspect").arg(&plain).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(field(&text, "layout:").as_deref(), Some("-"));
    assert!(!text.contains("reflowable"), "{text}");
    assert_eq!(field(&text, "cover:").as_deref(), Some("-"));
}

/// A book with two pictures in its first chapter: a 1×1 pixel named the
/// cover, and a diagram in the flow of text.
fn book_with_pictures(path: &Path) {
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>Pictures</dc:title><dc:language>en</dc:language>
<dc:identifier id="id">urn:pictures</dc:identifier>
</metadata>
<manifest>
<item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
<item id="cv" href="cover.png" media-type="image/png" properties="cover-image"/>
<item id="art" href="art.png" media-type="image/png"/>
</manifest>
<spine><itemref idref="c0"/></spine></package>"#;
    place(
        path,
        testkit::book_with(
            "cli-pictures",
            "Pictures",
            &[
                r#"<img src="cover.png" alt="cover"/><p>a diagram follows</p><img src="art.png" alt="art"/>"#,
            ],
            &[
                ("mimetype", b"application/epub+zip"),
                ("OEBPS/content.opf", opf.as_bytes()),
                ("OEBPS/cover.png", &testkit::png(1, 1, [200, 100, 50, 255])),
                ("OEBPS/art.png", &testkit::png(100, 50, [200, 100, 50, 255])),
            ],
        ),
    );
}

#[test]
fn images_reports_each_picture_and_its_own_size() {
    // The report is about the file, not about a screen: the size printed is the
    // picture's own, in pixels, so it does not move with the terminal's cells or
    // with how much room a view happens to give it.
    let dir = scratch("images-report");
    let file = dir.join("pictures.epub");
    book_with_pictures(&file);
    let file = file.to_str().unwrap().to_string();

    let out = omaread(&dir).args(["images", &file]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(" px"), "{text}");
    assert!(text.contains("readable"), "{text}");
    assert!(!text.contains("cells"), "no drawn size in a report about a file: {text}");
    assert!(!text.contains("fill("), "no rule column either: {text}");
}

/// A scratch installation where Omarchy is installed — an `omarchy` directory
/// beside the settings is how that shows — and the theme applier is stood in
/// for: it writes down the name it was handed, so a test sees which file
/// named the theme.
fn omarchy_scratch(name: &str) -> (Scratch, PathBuf) {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join("config/omarchy")).unwrap();
    // Where this Omarchy release records the current theme: the state
    // directory, per XDG_STATE_HOME.
    std::fs::create_dir_all(dir.join("state/omarchy/current")).unwrap();
    std::fs::write(dir.join("state/omarchy/current/theme.name"), "testtheme\n").unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let answer = dir.join("theme-applied");
    let applier = bin.join("omarchy-theme-set");
    std::fs::write(
        &applier,
        format!("#!/bin/sh\necho \"$@\" > {}\n", answer.display()),
    )
    .unwrap();
    std::fs::set_permissions(&applier, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, answer)
}

/// The program's first start, on the terminal `script` provides: the settings
/// file and the template are written only once the program owns one, and a
/// test has none to give.
fn first_start(dir: &Path) {
    let binary = format!("'{}'", env!("CARGO_BIN_EXE_omaread"));
    let mut terminal = Command::new("script");
    terminal.args(["-qec", &binary, "/dev/null"]);
    scratch_env(&mut terminal, dir);
    terminal.env(
        "PATH",
        format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap()),
    );
    let out = terminal.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn the_first_start_writes_every_file_under_the_xdg_variable_that_names_it() {
    // One environment decides every path the program touches. Each variable
    // names a directory of its own, and every file the first start writes —
    // the settings, the theme template — must land under the variable that
    // names it, not under the home directory where this machine's real files
    // live.
    let (dir, answer) = omarchy_scratch("xdg-config");
    first_start(&dir);

    // The settings file, under XDG_CONFIG_HOME, and its commented default
    // journal naming the directory under XDG_DATA_HOME.
    let config = dir.join("config/omaread/config.toml");
    let text = std::fs::read_to_string(&config).expect("the settings file follows XDG_CONFIG_HOME");
    assert!(
        text.contains(&dir.join("data/omaread/journal").display().to_string()),
        "the default journal is not under XDG_DATA_HOME:\n{text}"
    );

    // The theme template, under XDG_CONFIG_HOME beside it, written as Omarchy
    // renders it.
    let template = dir.join("config/omarchy/themed/omaread.toml.tpl");
    assert_eq!(
        std::fs::read_to_string(&template).expect("the template follows XDG_CONFIG_HOME"),
        include_str!("../themed/omaread.toml.tpl")
    );

    // And the theme applied was the one named under XDG_STATE_HOME.
    assert_eq!(
        std::fs::read_to_string(&answer)
            .expect("the current theme was applied")
            .trim(),
        "testtheme"
    );
}

#[test]
fn a_scan_writes_its_journal_under_xdg_data_home() {
    // The journal is the program's own data: a scan writes the single local
    // log under XDG_DATA_HOME.
    let dir = scratch("xdg-data");
    let file = dir.join("books/one.epub");
    book(&file, "One", &["first chapter text"]);
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let journal = dir.join("data/omaread/journal");
    assert!(
        journal.join("journal.jsonl").exists(),
        "no journal file under {}",
        journal.display()
    );
}

#[test]
fn embed_waits_for_reindex() {
    // `--embed` without `--reindex` used to be accepted and silently
    // ignored: nothing re-indexed, so nothing embedded. The refusal must
    // also leave the export folder untouched.
    let dir = scratch("embed");
    let out = omaread(&dir).args(["export", "--embed"]).output().unwrap();
    assert!(!out.status.success(), "--embed alone must be refused");
    assert!(
        !dir.join("data/omaread/export").exists(),
        "an export that was refused still wrote something"
    );
}

#[test]
fn an_export_writes_files_the_owner_only_can_read() {
    // The journal is written 0600, and an export says what somebody reads:
    // the files it creates are private the same way, not the 0644 a plain
    // write would leave behind.
    let dir = scanned("export-private");
    let target = dir.join("out");
    let out = omaread(&dir).arg("export").arg(&target).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let mut modes = Vec::new();
    for entry in std::fs::read_dir(&target).unwrap().flatten() {
        for chapter in std::fs::read_dir(entry.path()).unwrap().flatten() {
            modes.push(chapter.metadata().unwrap().permissions().mode() & 0o777);
        }
    }
    assert!(!modes.is_empty(), "the export wrote no chapters");
    let octal: Vec<String> = modes.iter().map(|mode| format!("{mode:o}")).collect();
    assert!(
        modes.iter().all(|mode| *mode == 0o600),
        "chapter files are {:?}, not 600",
        octal
    );
}

#[test]
fn the_author_names_can_be_read_and_corrected_in_bulk() {
    // The case an agent is handed: one author, written several ways across
    // books. `authors` shows the mess, `edit` fixes a whole spelling in one
    // call, and the JSON list form keeps a name that holds a comma whole.
    let dir = scratch("authors");
    book(&dir.join("books/one.epub"), "One", &["text"]);
    book(&dir.join("books/two.epub"), "Two", &["text"]);
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // Both files name their author the same way, and the view says so once.
    let out = omaread(&dir).args(["authors", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let authors: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(authors.as_array().map(Vec::len), Some(1), "{authors}");
    assert_eq!(authors[0]["author"], "Stephenson");
    assert_eq!(authors[0]["books"], 2);

    // One call corrects every book the word picks out, and says what it wrote.
    let out = omaread(&dir)
        .args([
            "edit",
            "Stephenson",
            r#"authors=["Neal Stephenson"]"#,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let changed: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(changed.as_array().map(Vec::len), Some(2), "{changed}");
    assert!(
        changed
            .as_array()
            .unwrap()
            .iter()
            .all(|book| book["authors"] == serde_json::json!(["Neal Stephenson"])),
        "{changed}"
    );

    // The view agrees, and the old spelling is gone.
    let out = omaread(&dir).args(["authors", "--json"]).output().unwrap();
    let authors: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(authors.as_array().map(Vec::len), Some(1), "{authors}");
    assert_eq!(authors[0]["author"], "Neal Stephenson");
    assert_eq!(authors[0]["books"], 2);

    // A name holding a comma survives the JSON list form: it is one author,
    // where `authors=Le Guin, Ursula` would have been read as two.
    let id = testkit::id_of(&dir.join("books/one.epub"));
    let out = omaread(&dir)
        .args(["set", &id[..12], r#"authors=["Le Guin, Ursula"]"#, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let book: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(book["authors"], serde_json::json!(["Le Guin, Ursula"]));
}
