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

/// A scratch directory, emptied first so each test starts from no library.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omaread-cli-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A minimal EPUB with one chapter per entry, which is all any of these
/// commands asks of a book.
fn book(path: &Path, title: &str, chapters: &[&str]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .unwrap();

    let items: String = (0..chapters.len())
        .map(|i| {
            format!("<item id=\"c{i}\" href=\"ch{i}.xhtml\" media-type=\"application/xhtml+xml\"/>")
        })
        .collect();
    let refs: String = (0..chapters.len())
        .map(|i| format!("<itemref idref=\"c{i}\"/>"))
        .collect();
    zip.start_file("OEBPS/content.opf", options).unwrap();
    write!(
        zip,
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title>{title}</dc:title><dc:creator>Stephenson</dc:creator>
<dc:language>en</dc:language><dc:identifier id="id">urn:test-{title}</dc:identifier>
</metadata>
<manifest>{items}</manifest><spine>{refs}</spine></package>"#
    )
    .unwrap();

    for (i, text) in chapters.iter().enumerate() {
        zip.start_file(format!("OEBPS/ch{i}.xhtml"), options)
            .unwrap();
        write!(
            zip,
            r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><h1>Chapter {n}</h1><p>{text}</p></body></html>"#,
            n = i + 1
        )
        .unwrap();
    }
    zip.finish().unwrap();
}

/// A scratch library with one two-chapter book in it.
fn scanned(name: &str) -> PathBuf {
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

/// The id the library gives a file: its content hash, as the reader computes it.
fn id_of(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap();
    let digest = Sha256::digest(bytes);
    let mut id = String::from("sha256:");
    for byte in digest.iter() {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

#[test]
fn the_scan_list_show_set_forget_round_trip() {
    let dir = scratch("round-trip");
    let file = dir.join("books/anathem.epub");
    book(
        &file,
        "Anathem",
        &["first chapter text", "second chapter text"],
    );
    let id = id_of(&file);

    // Nothing has been scanned: the empty shelf points at the scan that fills
    // it rather than at a flag the command no longer has.
    let out = omaread(&dir).arg("list").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("omaread scan"), "{}", stdout(&out));

    // scan → list: one file scanned is one book, counted in the singular.
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = omaread(&dir).arg("list").output().unwrap();
    assert!(stdout(&out).starts_with("1 book\n"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("1 books"), "{}", stdout(&out));

    // show: by the file itself, before anything was set by hand.
    let out = omaread(&dir).arg("show").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("Anathem"), "{}", stdout(&out));

    // set: a correction the file cannot make, which `show` then answers.
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
    for reference in ["Other", &id[..12]] {
        let out = omaread(&dir).arg("show").arg(reference).output().unwrap();
        assert!(out.status.success(), "{reference}: {}", stderr(&out));
        assert!(stdout(&out).contains("Other"), "{reference}: {out:?}",);
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

    // Where the reader stopped belongs to the record, and travels with it.
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
    let out = omaread(&dir).args(["list", "--json"]).output().unwrap();
    let books: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(books[0]["position"]["href"], "OEBPS/ch1.xhtml");
    assert_eq!(books[0]["position"]["block"], 2);

    // forget: the book goes, and the reading position goes with it — a scan
    // afterwards brings back a book with nothing recorded.
    let out = omaread(&dir).args(["forget", "Other"]).output().unwrap();
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
    std::fs::create_dir_all(&dir).unwrap();
    let archive = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
    let mut zip = archive;
    let options = zip::write::SimpleFileOptions::default();

    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .unwrap();
    zip.start_file("OEBPS/content.opf", options).unwrap();
    write!(
        zip,
        r#"<?xml version="1.0"?>
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
<spine><itemref idref="c0"/></spine></package>"#
    )
    .unwrap();
    zip.start_file("OEBPS/ch0.xhtml", options).unwrap();
    write!(
        zip,
        r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><p>a panel</p></body></html>"#
    )
    .unwrap();
    zip.finish().unwrap();

    let out = omaread(&dir).arg("inspect").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("layout:     pre-paginated"), "{text}");
    // The cover's path is resolved against the package document, like any
    // other href in it.
    assert!(text.contains("cover:      OEBPS/cover.png"), "{text}");

    // A book that declared nothing reads `-`, like any other absent value:
    // reflowable is the default, and printing it as a declaration would
    // claim more than the book said.
    let plain = dir.join("plain.epub");
    book(&plain, "Plain", &["first chapter text"]);
    let out = omaread(&dir).arg("inspect").arg(&plain).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("layout:     -"), "{text}");
    assert!(!text.contains("reflowable"), "{text}");
    assert!(text.contains("cover:      -"), "{text}");
}

/// A book with two pictures in its first chapter: a 1×1 pixel named the
/// cover, and a diagram in the flow of text.
fn book_with_pictures(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([200, 100, 50, 255]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
    )
    .unwrap();
    zip.start_file("OEBPS/content.opf", options).unwrap();
    write!(
        zip,
        r#"<?xml version="1.0"?>
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
<spine><itemref idref="c0"/></spine></package>"#
    )
    .unwrap();
    zip.start_file("OEBPS/ch0.xhtml", options).unwrap();
    write!(
        zip,
        r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><img src="cover.png" alt="cover"/><p>a diagram follows</p><img src="art.png" alt="art"/></body></html>"#
    )
    .unwrap();
    zip.start_file("OEBPS/cover.png", options).unwrap();
    zip.write_all(&png(1, 1)).unwrap();
    zip.start_file("OEBPS/art.png", options).unwrap();
    zip.write_all(&png(100, 50)).unwrap();
    zip.finish().unwrap();
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
    assert!(text.contains(" px  "), "{text}");
    assert!(text.contains("readable"), "{text}");
    assert!(!text.contains("cells"), "no drawn size in a report about a file: {text}");
    assert!(!text.contains("fill("), "no rule column either: {text}");
}

#[test]
fn the_xdg_variables_decide_where_every_file_goes() {
    // One environment decides every path the program touches. Each variable
    // names a directory of its own, and every file — the settings, the
    // journal, the theme template — must land under the variable that names
    // it, not under the home directory where this machine's real files live.
    let dir = scratch("xdg");

    // Omarchy only gets a template where Omarchy is installed, and an
    // `omarchy` directory beside the settings is how that shows.
    std::fs::create_dir_all(dir.join("config/omarchy")).unwrap();
    // Where this Omarchy release records the current theme: the state
    // directory, per XDG_STATE_HOME.
    std::fs::create_dir_all(dir.join("state/omarchy/current")).unwrap();
    std::fs::write(dir.join("state/omarchy/current/theme.name"), "testtheme\n").unwrap();
    // The theme applier, stood in for. The name it is handed is how this test
    // sees which file named the theme.
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

    // The settings file and the template are written by the first start, which
    // only runs once the program owns a terminal — and a test has none to
    // give. `script` hands the binary a pty; with an empty library the reader
    // leaves again at once.
    let binary = format!("'{}'", env!("CARGO_BIN_EXE_omaread"));
    let mut terminal = Command::new("script");
    terminal.args(["-qec", &binary, "/dev/null"]);
    scratch_env(&mut terminal, &dir);
    terminal.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    let out = terminal.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // The settings file, under XDG_CONFIG_HOME, and its commented default
    // journal naming the directory under XDG_DATA_HOME. Both exist, so the
    // first start ran.
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

    // A scan writes the journal under XDG_DATA_HOME, one file per machine.
    let file = dir.join("books/one.epub");
    book(&file, "One", &["first chapter text"]);
    let out = omaread(&dir).args(["scan", "books"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let journal = dir.join("data/omaread/journal");
    let written: Vec<_> = std::fs::read_dir(&journal)
        .expect("the journal follows XDG_DATA_HOME")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("journal-"))
        .collect();
    assert!(
        !written.is_empty(),
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
        !dir.join("data/export").exists(),
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
