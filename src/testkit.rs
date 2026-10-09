//! Scratch paths and fixture books for tests.
//!
//! A test that writes files leaves them in the temp dir. A name that does not
//! change between runs goes wrong twice: two `cargo test` runs share the name
//! and delete each other's fixtures, and every run leaves its files behind.
//! `Scratch` gives a test a directory named for this run and removes it when the
//! test ends, however it ends; `path` gives a file name the same way.
//!
//! The EPUB pieces below are shared by the tests that read books — the reader's
//! own tests, the chapter parser's and the command line's — so one container
//! format is written once and none can drift into a format the others do not
//! produce. Keys and flat pictures are here for the same reason.
//!
//! This module must stay standalone: `tests/cli.rs` pulls it in with `#[path]`
//! as its own crate, so it may not reach into `crate::` — everything it needs
//! comes from the dependencies both crates already share. Not every fixture
//! is used on both sides, so unused ones are no one's warning.
#![allow(dead_code)]

use image::DynamicImage;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A path under the temp dir, unique to this run so two runs cannot collide.
fn unique(tag: &str, extension: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let name = format!(
        "omaread-{tag}-{}-{}{extension}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    );
    std::env::temp_dir().join(name)
}

/// A temporary file path. The caller removes the file; the name is unique, so a
/// leftover cannot be mistaken for another run's fixture.
pub fn path(tag: &str, extension: &str) -> PathBuf {
    unique(tag, extension)
}

/// The id the library gives a book, computed the way a scan computes it:
/// `sha256:` and the hex of the content's digest. The one shape every test
/// that names a book by id has to agree with.
pub fn id_of(path: &Path) -> String {
    id_of_bytes(&std::fs::read(path).unwrap())
}

/// The id of a book made of these bytes, for a fixture written straight to a
/// file.
pub fn id_of_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut id = String::from("sha256:");
    for byte in digest.iter() {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// A book file that is there for the test and then gone: the file is removed
/// when this value is dropped, however the test ends, so a test that reads a
/// book leaves the temp dir as it found it and spends no line on the tidy-up.
pub struct TempBook(PathBuf);

impl TempBook {
    /// A book with one chapter per body.
    pub fn new(name: &str, title: &str, bodies: &[&str]) -> Self {
        Self(book(name, title, bodies))
    }

    /// A book with the entries a fixture of its own needs.
    pub fn with(
        name: &str,
        title: &str,
        bodies: &[&str],
        extra_entries: &[(&str, &[u8])],
    ) -> Self {
        Self(book_with(name, title, bodies, extra_entries))
    }
}

impl std::ops::Deref for TempBook {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for TempBook {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempBook {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A temporary directory, removed when the value is dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let dir = unique(tag, "");
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl std::ops::Deref for Scratch {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a container from its entries, so a test drives the reader the
/// way a person does: from a file on disk.
pub fn container(name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    use std::io::Write;
    let path = path(name, ".epub");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (entry, body) in entries {
        zip.start_file(*entry, options).unwrap();
        zip.write_all(body).unwrap();
    }
    zip.finish().unwrap();
    path
}

pub const CONTAINER: &[u8] = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// The package document for `chapters` chapters: the book's own name, the
/// author and identifier a test wants the file to carry — absent when the
/// test says nothing about them — and whatever extra manifest item it needs.
pub fn package(
    title: &str,
    author: Option<&str>,
    identifier: Option<&str>,
    chapters: usize,
    extra_item: &str,
) -> Vec<u8> {
    let items: String = (0..chapters)
        .map(|i| {
            format!(
                "  <item id=\"c{i}\" href=\"ch{i}.xhtml\" media-type=\"application/xhtml+xml\"/>"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let spine: String = (0..chapters)
        .map(|i| format!("  <itemref idref=\"c{i}\"/>"))
        .collect::<Vec<_>>()
        .join("\n");
    let creator = author
        .map(|author| format!("<dc:creator>{author}</dc:creator>"))
        .unwrap_or_default();
    let identity = identifier
        .map(|identifier| format!("<dc:identifier id=\"id\">{identifier}</dc:identifier>"))
        .unwrap_or_default();
    let unique = if identifier.is_some() {
        " unique-identifier=\"id\""
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0"{unique}>
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title>{creator}{identity}</metadata>
<manifest>
{items}
  {extra_item}
</manifest>
<spine>
{spine}
</spine></package>"#
    )
    .into_bytes()
}

/// A package document for a one-chapter book, with whatever extra manifest
/// item the test needs.
pub fn opf(title: &str, extra_item: &str) -> Vec<u8> {
    package(title, None, None, 1, extra_item)
}

pub fn chapter_xhtml(body: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body>{body}</body></html>"#
    )
    .into_bytes()
}

/// A book with one chapter per body: the fixture a test reads, built so no
/// test has to spell out the container, the package document and the chapter
/// wrapper again.
pub fn book(name: &str, title: &str, bodies: &[&str]) -> PathBuf {
    book_with(name, title, bodies, &[])
}

/// A book whose file names its own author and identifier — the fixture for a
/// test that reads the metadata back out, or names the book by the id the
/// file carries.
pub fn named_book(
    name: &str,
    title: &str,
    author: &str,
    identifier: &str,
    bodies: &[&str],
) -> PathBuf {
    let opf = package(title, Some(author), Some(identifier), bodies.len(), "");
    book_with(name, title, bodies, &[("OEBPS/content.opf", &opf)])
}

/// A one-chapter book of many short paragraphs, so the text runs past a
/// single screen — the book a view test scrolls.
pub fn text_book(name: &str, title: &str, paragraphs: usize) -> TempBook {
    let mut body = format!("<h1>{title}</h1>");
    for i in 0..paragraphs {
        body.push_str(&format!(
            "<p>Paragraph {i} carries enough words to wrap the line once in a while.</p>"
        ));
    }
    TempBook::new(name, title, &[&body])
}

/// A book with one chapter per body, plus the entries a fixture of its own
/// needs. An entry that names a standard file takes its place rather than
/// doubling up: the package document is how a test brings its own manifest —
/// an author, an item no file backs — while the chapters still come from
/// here.
pub fn book_with(
    name: &str,
    title: &str,
    bodies: &[&str],
    extra_entries: &[(&str, &[u8])],
) -> PathBuf {
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("META-INF/container.xml".into(), CONTAINER.to_vec()),
        (
            "OEBPS/content.opf".into(),
            package(title, None, None, bodies.len(), ""),
        ),
    ];
    for (index, body) in bodies.iter().enumerate() {
        entries.push((format!("OEBPS/ch{index}.xhtml"), chapter_xhtml(body)));
    }
    for (entry, body) in extra_entries {
        match entries
            .iter_mut()
            .find(|(known, _)| known.as_str() == *entry)
        {
            Some(slot) => slot.1 = body.to_vec(),
            None => entries.push(((*entry).to_string(), body.to_vec())),
        }
    }
    let entries: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(entry, body)| (entry.as_str(), body.as_slice()))
        .collect();
    container(name, &entries)
}

/// A picture of one flat colour: the shape every fixture picture is cut to,
/// so a size and a colour are all a test has to name.
pub fn image(width: u32, height: u32, rgba: [u8; 4]) -> DynamicImage {
    DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba(rgba),
    ))
}

/// The same picture encoded, for a test that drives the reader from bytes.
pub fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image(width, height, rgba)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

/// A PNG whose header measures `width` x `height` but whose pixels are gone:
/// cut just past the IDAT chunk's own header, so the size still reads out of
/// it and no decoder gets the picture back. The fixture a test uses when it
/// needs a picture that measures and cannot be drawn.
pub fn broken_png(width: u32, height: u32) -> Vec<u8> {
    let png = png(width, height, [10, 20, 30, 255]);
    let idat = png
        .windows(4)
        .position(|window| window == b"IDAT")
        .expect("a PNG has an IDAT chunk");
    png[..idat + 8].to_vec()
}

/// A key press as the reader takes one: pressed once, nothing held — the
/// state crossterm sends on a plain key, so a test says only which key.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// A key press with Ctrl held.
pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(KeyCode::Char(c))
    }
}
