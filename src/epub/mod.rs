//! Reading EPUB containers: package document, spine and navigation.

pub mod mathml;
pub mod xhtml;

use crate::doc::Chapter;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Metadata taken from the package document. Only fields the reader shows.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    /// `dc:identifier`, used later to match two files of the same book.
    pub identifier: Option<String>,
}

/// One entry of the reading order.
#[derive(Debug, Clone)]
pub struct SpineItem {
    /// Path inside the container, relative to its root.
    pub href: String,
    /// Title from the navigation document, when one points here.
    pub title: Option<String>,
}

/// One manifest entry: where the file is, and what the publisher declared
/// about it.
#[derive(Debug, Clone)]
struct ManifestItem {
    /// Href as written in the package document, resolved wherever it is used.
    href: String,
    /// What the manifest says the entry is. The cover has to be a picture to
    /// be drawn as one, and the media type is how a page holding a cover is
    /// told apart from the cover itself.
    media_type: String,
    /// The space-separated `properties` attribute, split into tokens. EPUB 3
    /// marks the cover with `cover-image` and the table of contents with `nav`,
    /// and throwing the rest away would mean re-parsing to ask.
    properties: Vec<String>,
}

pub struct Book {
    archive: ZipArchive<File>,
    /// Directory of the package document; hrefs resolve against it.
    root: PathBuf,
    pub metadata: Metadata,
    pub spine: Vec<SpineItem>,
    /// Container path of the cover — always a *picture* — when the publication
    /// names one: by `properties="cover-image"`, by an EPUB 2 `<meta
    /// name="cover">`, by a `guide` reference, or by a landmark in the
    /// navigation document. A guide reference and a landmark name an XHTML
    /// page that holds the cover, so `cover_picture` follows a page to its
    /// first picture; what does not resolve to a picture is no cover, and
    /// reads as none rather than as one that is never drawn.
    pub cover: Option<String>,
    /// The layout the package document declared — `pre-paginated` or
    /// `reflowable`. `None` when it declared nothing, which is the common
    /// case: reflowable is the default, and a report that printed it as
    /// though the book had said so claimed more than the book did.
    pub layout: Option<String>,
}

impl Book {
    pub fn open(path: &Path) -> Result<Self> {
        // A file's name may hold anything, and this message ends up printed:
        // the terminal obeys what it is given, so it is cleaned on the way out.
        let shown_path = crate::journal::clean(&path.display().to_string());
        let file = File::open(path).with_context(|| format!("cannot open {shown_path}"))?;
        let mut archive =
            ZipArchive::new(file).with_context(|| format!("{shown_path} is not a zip archive"))?;

        let opf_path = find_package_path(&mut archive)?;
        let root = Path::new(&opf_path)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();

        let opf = read_entry(&mut archive, &opf_path)?;
        let package = parse_package(&opf)?;

        let mut spine = Vec::new();
        for idref in &package.spine {
            if let Some(item) = package.manifest.get(idref) {
                spine.push(SpineItem {
                    href: normalize(&root, &item.href),
                    title: None,
                });
            }
        }
        if spine.is_empty() {
            bail!("package document lists no readable spine items");
        }

        // What each manifest entry is, by the path it resolves to: the cover
        // is followed to a picture through this.
        let media: HashMap<String, String> = package
            .manifest
            .values()
            .map(|item| (normalize(&root, &item.href), item.media_type.clone()))
            .collect();
        // The cover's href comes from the package document, so it resolves
        // against the package document like every other href in it.
        let cover = package.cover.as_deref().map(|href| normalize(&root, href));
        let mut book = Self {
            archive,
            root,
            metadata: package.metadata,
            cover,
            layout: package.layout,
            spine,
        };
        book.apply_navigation(&package.nav_href, &package.ncx_href);
        // After navigation has had its say — its landmarks are the last place
        // a cover is named — the cover has to be a picture to be drawn as one.
        if let Some(named) = book.cover.take() {
            book.cover = cover_picture(&named, &media, &mut book.archive);
        }
        Ok(book)
    }

    /// Parses the chapter at `index` of the reading order.
    pub fn chapter(&mut self, index: usize) -> Result<Chapter> {
        let item = self
            .spine
            .get(index)
            .ok_or_else(|| anyhow!("no spine item at index {index}"))?
            .clone();
        let source = read_entry(&mut self.archive, &item.href)?;
        // Image paths in a chapter are relative to the chapter's own directory.
        let base = Path::new(&item.href)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = xhtml::parse_in(&source, &base)
            .with_context(|| format!("cannot parse chapter {}", item.href))?;
        Ok(Chapter {
            href: item.href,
            blocks: parsed.blocks,
            links: parsed.links,
            anchors: parsed.anchors,
        })
    }

    pub fn title(&self) -> &str {
        self.metadata.title.as_deref().unwrap_or("Untitled")
    }

    /// What one of its pictures is, which is what decides how big it is drawn.
    ///
    /// The cover fills the room; in a pre-paginated book every picture is a
    /// page and fills it too; everything else is a picture in the flow of text,
    /// where a mark keeps its own size and an illustration is magnified into
    /// the room. `src` is the picture's path as the chapter
    /// spells it — a container path, the same form the cover's own href
    /// resolves to, so the two can be compared directly. The cover is asked
    /// for first, so a cover in a pre-paginated book is still reported as the
    /// cover — the rule is the same either way, the reason is not.
    pub fn picture_reason(&self, src: &str) -> crate::image::Reason {
        if self.cover.as_deref() == Some(src) {
            crate::image::Reason::Cover
        } else if self.layout.as_deref() == Some("pre-paginated") {
            crate::image::Reason::Page
        } else {
            crate::image::Reason::Flow
        }
    }

    /// Raw bytes of a file inside the container, for images.
    ///
    /// Capped like every other entry: a picture nobody can draw is not worth
    /// gigabytes of memory, and the decoder refuses a bomb of its own.
    pub fn read_binary(&mut self, name: &str) -> Result<Vec<u8>> {
        read_entry_bytes(&mut self.archive, name, MAX_ENTRY_BYTES)
    }

    /// The first bytes of an entry — enough to read a picture's header.
    ///
    /// The layout measures every picture of a chapter on every re-layout, so
    /// reading whole entries there re-read megabytes on every window resize.
    /// 64 KB is generous: PNG, JPEG, GIF and WebP all declare their size
    /// within it.
    pub fn read_header(&mut self, name: &str) -> Result<Vec<u8>> {
        read_prefix(&mut self.archive, name, HEADER_BYTES)
            .with_context(|| format!("{name} is missing from the container"))
    }

    /// Fills in spine titles from the navigation document, preferring EPUB 3's
    /// nav over EPUB 2's NCX, and picks up the cover from the nav's landmarks
    /// when nothing else named it.
    fn apply_navigation(&mut self, nav_href: &Option<String>, ncx_href: &Option<String>) {
        let mut titles = None;
        if let Some(href) = nav_href {
            let full = normalize(&self.root.clone(), href);
            if let Ok(xml) = read_entry(&mut self.archive, &full) {
                // The landmarks are the last of the places readers look for a
                // cover, and only worth asking when the others stayed silent.
                if self.cover.is_none() {
                    self.cover = parse_cover_landmark(&xml, &full);
                }
                titles = parse_nav(&xml, &full).ok();
            }
        }
        let titles = titles.or_else(|| {
            ncx_href.as_ref().and_then(|href| {
                let full = normalize(&self.root.clone(), href);
                read_entry(&mut self.archive, &full)
                    .ok()
                    .and_then(|xml| parse_ncx(&xml, &full).ok())
            })
        });

        let Some(titles) = titles else { return };
        for item in &mut self.spine {
            if let Some(title) = titles.get(&item.href) {
                item.title = Some(title.clone());
            }
        }
    }
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    Ok(decode(read_entry_bytes(archive, name, MAX_ENTRY_BYTES)?))
}

/// Reads at most `limit` bytes of one entry, naming the entry when there are
/// more than that.
///
/// A whole entry used to be read with `read_to_end`, and a 438 KB book
/// holding a 300 MB chapter took `inspect` to 2.6 GB resident. An entry that
/// claims more than the limit is not a chapter or a package document but
/// something to refuse, and the message says which entry it was.
fn read_entry_bytes(archive: &mut ZipArchive<File>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let buffer = read_prefix(archive, name, limit.saturating_add(1))?;
    if buffer.len() as u64 > limit {
        let limit = if limit >= 1024 * 1024 {
            format!("{} MB", limit / (1024 * 1024))
        } else {
            format!("{} KB", limit / 1024)
        };
        bail!("{name} is larger than the {limit} this reader takes from one entry");
    }
    Ok(buffer)
}

/// Reads at most `limit` bytes of one entry: the start of it, whether or not
/// there is more.
///
/// A picture's entry is often far larger than its header, and a header read
/// must not read the picture — measuring every picture of a chapter on every
/// re-layout would otherwise re-read megabytes on every window resize.
fn read_prefix(archive: &mut ZipArchive<File>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("{name} is missing from the container"))?;
    let mut buffer = Vec::new();
    entry.by_ref().take(limit).read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// The most this reader takes from any one entry of a container: a chapter
/// and a package document are counted in kilobytes, so 32 MB is far past
/// what either can honestly need.
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;

/// The most this reader takes when it only wants an entry's header.
const HEADER_BYTES: u64 = 64 * 1024;

/// The cover as a picture, whichever of the book's places named it.
///
/// The manifest's own marks (`properties="cover-image"`, the EPUB 2 meta)
/// name the picture itself; a `guide` reference and an EPUB 3 landmark name
/// an XHTML *page* that holds it — and a page compared against an `<img src>`
/// never matches, which is why covers named that way were never drawn. So a
/// cover that is not already a picture is opened and its first picture taken
/// instead. A cover that resolves to anything else — a page with no picture,
/// a media type that is not an image — is not a cover this reader can draw,
/// and reads as none at all rather than as a cover that never appears.
fn cover_picture(
    named: &str,
    media: &HashMap<String, String>,
    archive: &mut ZipArchive<File>,
) -> Option<String> {
    if is_picture(media, archive, named) {
        return Some(named.to_string());
    }
    let picture = first_picture_of(archive, named).ok()?;
    is_picture(media, archive, &picture).then_some(picture)
}

/// Whether a container path holds a picture: the manifest says so, or — for
/// a resource the manifest does not name — the bytes measure.
fn is_picture(media: &HashMap<String, String>, archive: &mut ZipArchive<File>, path: &str) -> bool {
    match media.get(path) {
        Some(kind) => kind.starts_with("image/"),
        None => read_prefix(archive, path, HEADER_BYTES)
            .ok()
            .is_some_and(|bytes| crate::image::dimensions(&bytes).is_ok()),
    }
}

/// The first picture an XHTML page holds, as a container path.
fn first_picture_of(archive: &mut ZipArchive<File>, page: &str) -> Result<String> {
    let source = read_entry(archive, page)?;
    let base = Path::new(page)
        .parent()
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .into_owned();
    let parsed = xhtml::parse_in(&source, &base).with_context(|| format!("cannot parse {page}"))?;
    parsed
        .blocks
        .iter()
        .find_map(|block| match &block.kind {
            crate::doc::BlockKind::Image { src: Some(src) } => Some(src.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("{page} holds no picture to draw a cover from"))
}

/// Decodes bytes as UTF-8, replacing invalid sequences rather than failing.
fn decode(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    }
}

/// Resolves a URI reference against the directory it appears in, and returns the
/// path of that entry inside the container.
///
/// An href is a URI: a name holding a space or a character outside ASCII is
/// written percent-encoded, so a picture called `海报.png` arrives as
/// `%E6%B5%B7%E6%8A%A5.png`. Entries in the container are named as the bytes
/// themselves, so the decoding belongs here, once, on the way in. A picture
/// nobody can look up is a picture drawn as nothing, and the report of it names
/// a path nobody recognises.
pub(crate) fn container_path(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let decoded = percent_decode(href);
    let combined = if base.is_empty() || decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else {
        format!("{base}/{decoded}")
    };

    // Collapse `.` and `..` without touching the filesystem.
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Resolves an href from the package document against the container root.
fn normalize(root: &Path, href: &str) -> String {
    container_path(&root.to_string_lossy(), href)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_package_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let container = read_entry(archive, "META-INF/container.xml")
        .context("not an EPUB: META-INF/container.xml is missing")?;
    let doc = roxmltree::Document::parse_with_options(&container, xhtml::parsing_options())
        .context("container.xml is malformed")?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .map(percent_decode)
        .ok_or_else(|| anyhow!("container.xml names no package document"))
}

struct Package {
    metadata: Metadata,
    /// Manifest item id to its entry: where it points, and the properties it
    /// declares.
    manifest: HashMap<String, ManifestItem>,
    /// Spine idrefs in reading order.
    spine: Vec<String>,
    nav_href: Option<String>,
    ncx_href: Option<String>,
    /// Href of the cover as the package document spells it, not yet resolved.
    cover: Option<String>,
    /// The layout the package document declared, if it declared one.
    layout: Option<String>,
}

fn parse_package(xml: &str) -> Result<Package> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("package document is malformed")?;
    let mut metadata = Metadata::default();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut nav_href = None;
    let mut ncx_href = None;
    let mut spine_toc_id = None;
    // The three places a cover can be named, kept apart so the manifest's own
    // word outranks the older fallbacks whichever order they appear in.
    let mut epub2_cover_id = None;
    let mut guide_cover = None;
    let mut layout: Option<String> = None;

    for node in doc.descendants().filter(|n| n.is_element()) {
        let name = node.tag_name().name();
        match name {
            "title" if metadata.title.is_none() => metadata.title = text_of(node),
            "creator" => {
                if let Some(text) = text_of(node) {
                    metadata.authors.push(text);
                }
            }
            "language" if metadata.language.is_none() => metadata.language = text_of(node),
            "identifier" if metadata.identifier.is_none() => metadata.identifier = text_of(node),
            "meta" => {
                // EPUB 2 names the cover by the id of the manifest item that
                // holds the picture.
                if node.attribute("name") == Some("cover") {
                    epub2_cover_id = node.attribute("content").map(str::to_string);
                }
                // EPUB 3 often says the layout here rather than on the package
                // element. Only what the book actually said counts: a report
                // that printed a default as a declaration claimed more than
                // the book did.
                if node.attribute("property") == Some("rendition:layout") {
                    declare_layout(&mut layout, text_of(node).as_deref());
                }
            }
            "item" => {
                let id = node.attribute("id");
                let href = node.attribute("href");
                if let (Some(id), Some(href)) = (id, href) {
                    let properties: Vec<String> = node
                        .attribute("properties")
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    if properties.iter().any(|p| p == "nav") {
                        nav_href = Some(href.to_string());
                    }
                    if node.attribute("media-type") == Some("application/x-dtbncx+xml") {
                        ncx_href = Some(href.to_string());
                    }
                    manifest.insert(
                        id.to_string(),
                        ManifestItem {
                            href: href.to_string(),
                            media_type: node
                                .attribute("media-type")
                                .unwrap_or_default()
                                .to_string(),
                            properties,
                        },
                    );
                }
            }
            "spine" => {
                spine_toc_id = node.attribute("toc").map(str::to_string);
            }
            "itemref" => {
                if let Some(idref) = node.attribute("idref") {
                    // `linear="no"` marks material outside the reading order.
                    if node.attribute("linear") != Some("no") {
                        spine.push(idref.to_string());
                    }
                }
            }
            // A guide reference is EPUB 2's way of pointing at the cover; its
            // type says what it points at. The type is matched exactly: a type
            // that merely *contains* `cover` — `my-coverish` — points at some
            // other section of the book, and believing it made a stranger's
            // section the cover.
            "reference"
                if node
                    .attribute("type")
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("cover"))
                => {
                    guide_cover = node.attribute("href").map(str::to_string);
                }
            _ => {}
        }
    }

    // The package element itself can carry the layout, whichever prefix the
    // publisher bound the reserved name to.
    for attribute in doc.root_element().attributes() {
        if attribute.name() == "layout" {
            declare_layout(&mut layout, Some(attribute.value()));
        }
    }

    // EPUB 3 marks the cover in the manifest; the EPUB 2 meta and the guide
    // reference are fallbacks for the books that predate it.
    let cover = manifest
        .values()
        .find(|item| item.properties.iter().any(|p| p == "cover-image"))
        .map(|item| item.href.clone())
        .or_else(|| {
            epub2_cover_id
                .as_ref()
                .and_then(|id| manifest.get(id))
                .map(|item| item.href.clone())
        })
        .or(guide_cover);

    // EPUB 2 points at the NCX through the spine's `toc` attribute.
    if ncx_href.is_none()
        && let Some(id) = spine_toc_id {
            ncx_href = manifest.get(&id).map(|item| item.href.clone());
        }

    Ok(Package {
        metadata,
        manifest,
        spine,
        nav_href,
        ncx_href,
        cover,
        layout,
    })
}

/// Records a declared layout.
///
/// `pre-paginated` and `reflowable` are the two values the specification
/// allows, and only a declaration counts: a book that said nothing has no
/// layout to report. `pre-paginated` wins over a reflowable said elsewhere,
/// because that is the declaration that changes how the book is drawn.
fn declare_layout(declared: &mut Option<String>, value: Option<&str>) {
    match value.map(str::trim) {
        Some("pre-paginated") => *declared = Some("pre-paginated".to_string()),
        Some("reflowable") if declared.is_none() => {
            *declared = Some("reflowable".to_string());
        }
        _ => {}
    }
}

fn text_of(node: roxmltree::Node) -> Option<String> {
    let text: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Maps chapter paths to titles, taken from an EPUB 3 navigation document.
fn parse_nav(xml: &str, nav_path: &str) -> Result<HashMap<String, String>> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("navigation document is malformed")?;
    let base = Path::new(nav_path).parent().unwrap_or(Path::new(""));
    let mut titles = HashMap::new();

    let toc = doc
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "nav"
                && n.attributes()
                    .any(|a| a.name() == "type" && a.value() == "toc")
        })
        .or_else(|| {
            doc.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "nav")
        });
    let Some(toc) = toc else { return Ok(titles) };

    for anchor in toc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "a")
    {
        if let (Some(href), Some(title)) = (anchor.attribute("href"), text_of(anchor)) {
            titles.entry(normalize(base, href)).or_insert(title);
        }
    }
    Ok(titles)
}

/// The cover's href from an EPUB 3 navigation document's landmarks.
///
/// The landmarks list where each part of the book begins — cover, toc, text —
/// and the landmark typed `cover` points at the cover the way a guide
/// reference does. This is the last of the places readers look, so the caller
/// asks only when nothing else named the cover.
fn parse_cover_landmark(xml: &str, nav_path: &str) -> Option<String> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options()).ok()?;
    let base = Path::new(nav_path).parent().unwrap_or(Path::new(""));
    let landmarks = doc.descendants().find(|n| {
        n.is_element()
            && n.tag_name().name() == "nav"
            && n.attributes()
                .any(|a| a.name() == "type" && a.value() == "landmarks")
    })?;
    let href = landmarks
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "a"
                && n.attributes()
                    .any(|a| a.name() == "type" && a.value().to_ascii_lowercase().contains("cover"))
        })
        .and_then(|anchor| anchor.attribute("href"))?;
    Some(normalize(base, href))
}

/// Maps chapter paths to titles, taken from an EPUB 2 NCX document.
fn parse_ncx(xml: &str, ncx_path: &str) -> Result<HashMap<String, String>> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("NCX document is malformed")?;
    let base = Path::new(ncx_path).parent().unwrap_or(Path::new(""));
    let mut titles = HashMap::new();

    for point in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "navPoint")
    {
        let href = point
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "content")
            .and_then(|n| n.attribute("src"));
        let title = point
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "text")
            .and_then(text_of);
        if let (Some(href), Some(title)) = (href, title) {
            titles.entry(normalize(base, href)).or_insert(title);
        }
    }
    Ok(titles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs_resolve_against_their_directory() {
        assert_eq!(
            normalize(Path::new("OEBPS"), "ch01.xhtml"),
            "OEBPS/ch01.xhtml"
        );
        assert_eq!(
            normalize(Path::new("OEBPS/text"), "../ch01.xhtml"),
            "OEBPS/ch01.xhtml"
        );
        assert_eq!(normalize(Path::new(""), "ch01.xhtml"), "ch01.xhtml");
        // A fragment is not part of the file's name, and an escape is the
        // name itself — the bytes, not the percent signs.
        assert_eq!(normalize(Path::new(""), "ch01.xhtml#part2"), "ch01.xhtml");
        assert_eq!(normalize(Path::new(""), "a%20b.xhtml"), "a b.xhtml");
        assert_eq!(
            normalize(Path::new("OEBPS/Images"), "%E6%B5%B7%E6%8A%A5.png"),
            "OEBPS/Images/海报.png"
        );
        assert_eq!(normalize(Path::new(""), "%2E%2E/a.xhtml"), "a.xhtml");
    }

    #[test]
    fn reads_metadata_and_reading_order() {
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>A Book</dc:title>
            <dc:creator>An Author</dc:creator>
            <dc:language>en</dc:language>
            <dc:identifier>urn:uuid:1234</dc:identifier>
          </metadata>
          <manifest>
            <item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/>
            <item id="c2" href="ch02.xhtml" media-type="application/xhtml+xml"/>
            <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
            <item id="nav" href="nav.xhtml" properties="nav" media-type="application/xhtml+xml"/>
          </manifest>
          <spine>
            <itemref idref="c1"/>
            <itemref idref="cover" linear="no"/>
            <itemref idref="c2"/>
          </spine>
        </package>"#;
        let package = parse_package(opf).unwrap();
        assert_eq!(package.metadata.title.as_deref(), Some("A Book"));
        assert_eq!(package.metadata.authors, vec!["An Author"]);
        assert_eq!(
            package.metadata.identifier.as_deref(),
            Some("urn:uuid:1234")
        );
        assert_eq!(package.spine, vec!["c1", "c2"]);
        assert_eq!(package.nav_href.as_deref(), Some("nav.xhtml"));
        // A book that says nothing about covers or layouts has no cover to
        // show and no layout to report, which is the common case: `-` is the
        // honest answer for it, not a default dressed up as a declaration.
        assert!(package.cover.is_none());
        assert_eq!(package.layout, None);
    }

    #[test]
    fn a_cover_is_found_whichever_way_the_book_names_it() {
        // EPUB 3's own way: `properties` on the manifest item, among whatever
        // else that item declares. It outranks the older ways when a book
        // carries more than one.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Manga</dc:title>
            <meta name="cover" content="old"/>
          </metadata>
          <manifest>
            <item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/>
            <item id="old" href="old-cover.jpg" media-type="image/jpeg"/>
            <item id="cv" href="Images/cover.png" media-type="image/png" properties="scripted cover-image"/>
          </manifest>
          <spine><itemref idref="c1"/></spine>
          <guide><reference type="cover" href="cover.xhtml" title="Cover"/></guide>
        </package>"#;
        let package = parse_package(opf).unwrap();
        assert_eq!(package.cover.as_deref(), Some("Images/cover.png"));

        // EPUB 2 predates `properties`: the metadata names the id of the
        // manifest item that holds the picture, and the href comes from there.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Old Book</dc:title>
            <meta name="cover" content="cv"/>
          </metadata>
          <manifest>
            <item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/>
            <item id="cv" href="cover.jpg" media-type="image/jpeg"/>
          </manifest>
          <spine><itemref idref="c1"/></spine>
        </package>"#;
        let package = parse_package(opf).unwrap();
        assert_eq!(package.cover.as_deref(), Some("cover.jpg"));

        // The oldest way to say it: a `guide` reference typed `cover`, pointing
        // at whatever holds the cover. Its href is kept as written here and
        // resolved against the package document's directory on the way out of
        // `Book::open`, where a page is followed to its first picture.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Guided</dc:title>
          </metadata>
          <manifest>
            <item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine><itemref idref="c1"/></spine>
          <guide><reference type="cover" href="text/cover.xhtml" title="Cover"/></guide>
        </package>"#;
        let package = parse_package(opf).unwrap();
        assert_eq!(package.cover.as_deref(), Some("text/cover.xhtml"));

        // A type that merely contains the word points at something else — a
        // `my-coverish` section is not a cover, and believing it made a
        // stranger's section the cover.
        for kind in ["my-coverish", "cover-page", "covers", ""] {
            let opf = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Guided</dc:title></metadata>
          <manifest><item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/></manifest>
          <spine><itemref idref="c1"/></spine>
          <guide><reference type="{kind}" href="text/other.xhtml" title="Other"/></guide>
        </package>"#
            );
            let package = parse_package(&opf).unwrap();
            assert_eq!(package.cover, None, "type {kind:?} is not a cover");
        }

        // And the last place readers look: the landmarks of the navigation
        // document, for books that mark nothing in the manifest.
        let nav = r#"<html xmlns:epub="http://www.idpf.org/2007/ops">
          <body><nav epub:type="landmarks"><ol>
            <li><a epub:type="cover" href="cover.xhtml">Cover</a></li>
            <li><a epub:type="text" href="text/ch01.xhtml">Start</a></li>
          </ol></nav></body></html>"#;
        assert_eq!(
            parse_cover_landmark(nav, "OEBPS/nav.xhtml").as_deref(),
            Some("OEBPS/cover.xhtml")
        );
        // A nav without landmarks, or without a cover among them, names
        // nothing — and nothing is what the caller asked for.
        let toc = r#"<html xmlns:epub="http://www.idpf.org/2007/ops">
          <body><nav epub:type="toc"><ol><li><a href="ch01.xhtml">One</a></li></ol></nav></body></html>"#;
        assert!(parse_cover_landmark(toc, "OEBPS/nav.xhtml").is_none());
    }

    #[test]
    fn a_pre_paginated_book_is_read_whichever_way_it_says() {
        // The attribute on the package element, under whichever prefix the
        // publisher bound the reserved name to.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf"
                     xmlns:rendition="http://www.idpf.org/2007/ops"
                     version="3.0" rendition:layout="pre-paginated">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Panels</dc:title></metadata>
          <manifest><item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/></manifest>
          <spine><itemref idref="c1"/></spine>
        </package>"#;
        assert_eq!(
            parse_package(opf).unwrap().layout.as_deref(),
            Some("pre-paginated")
        );

        // And the metadata form, which is where EPUB 3.3 puts it.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Panels</dc:title>
            <meta property="rendition:layout">pre-paginated</meta>
          </metadata>
          <manifest><item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/></manifest>
          <spine><itemref idref="c1"/></spine>
        </package>"#;
        assert_eq!(
            parse_package(opf).unwrap().layout.as_deref(),
            Some("pre-paginated")
        );

        // A book that says `reflowable` says what is already the default —
        // and it said it, which is what a report shows.
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>Words</dc:title>
            <meta property="rendition:layout">reflowable</meta>
          </metadata>
          <manifest><item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/></manifest>
          <spine><itemref idref="c1"/></spine>
        </package>"#;
        assert_eq!(
            parse_package(opf).unwrap().layout.as_deref(),
            Some("reflowable")
        );
    }

    #[test]
    fn chapter_titles_are_read_from_the_navigation() {
        // The navigation document of an EPUB 3 …
        let nav = r#"<html xmlns:epub="http://www.idpf.org/2007/ops">
          <body><nav epub:type="toc"><ol>
            <li><a href="ch01.xhtml">First</a></li>
            <li><a href="ch02.xhtml#top">Second</a></li>
          </ol></nav></body></html>"#;
        let titles = parse_nav(nav, "OEBPS/nav.xhtml").unwrap();
        assert_eq!(
            titles.get("OEBPS/ch01.xhtml").map(String::as_str),
            Some("First")
        );
        assert_eq!(
            titles.get("OEBPS/ch02.xhtml").map(String::as_str),
            Some("Second")
        );

        // … and the NCX of an EPUB 2, under the same names.
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
          <navPoint><navLabel><text>Chapter One</text></navLabel>
            <content src="ch01.xhtml"/></navPoint>
        </navMap></ncx>"#;
        let titles = parse_ncx(ncx, "OEBPS/toc.ncx").unwrap();
        assert_eq!(
            titles.get("OEBPS/ch01.xhtml").map(String::as_str),
            Some("Chapter One")
        );
    }

    /// Writes a container with the given entries, so `Book::open` can be
    /// driven the way a reader drives it — from a file on disk.
    fn container(name: &str, entries: &[(&str, &str)]) -> PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("omaread-epub-{name}.epub"));
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    const CONTAINER_XML: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    const CHAPTER: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><p>text</p></body></html>"#;

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>One</dc:title></metadata>
<manifest><item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="c0"/></spine></package>"#;

    #[test]
    fn a_cover_named_by_a_guide_is_followed_to_its_picture_or_gives_up() {
        // A `guide` reference names an XHTML *page*; compared against an
        // `<img src>` it never matched, so those covers were never drawn —
        // most books name the cover this way. The page is opened and its
        // first picture taken, and that is the cover from here on.
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Guided</dc:title></metadata>
<manifest>
  <item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
  <item id="pg" href="text/cover.xhtml" media-type="application/xhtml+xml"/>
  <item id="cv" href="images/cover.png" media-type="image/png"/>
</manifest>
<spine><itemref idref="c0"/></spine>
<guide><reference type="cover" href="text/cover.xhtml" title="Cover"/></guide>
</package>"#;
        let page = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><img src="../images/cover.png" alt="Cover"/><p>back</p></body></html>"#;
        let path = container(
            "guide-cover",
            &[
                ("META-INF/container.xml", CONTAINER_XML),
                ("OEBPS/content.opf", opf),
                ("OEBPS/ch0.xhtml", CHAPTER),
                ("OEBPS/text/cover.xhtml", page),
                // Never opened here: the cover is followed by its media type.
                ("OEBPS/images/cover.png", "not really a png"),
            ],
        );
        let book = Book::open(&path).unwrap();
        assert_eq!(book.cover.as_deref(), Some("OEBPS/images/cover.png"));
        // The picture the page held is classified as the cover, so the size
        // rule for covers finally has something to match.
        assert_eq!(
            book.picture_reason("OEBPS/images/cover.png"),
            crate::image::Reason::Cover
        );
        assert_eq!(
            book.picture_reason("OEBPS/ch0.xhtml"),
            crate::image::Reason::Flow
        );
        std::fs::remove_file(path).ok();

        // Only a cover that resolves to something with an image media type is
        // kept. A page with no picture in it reads as `-` in a report, rather
        // than as a cover that is never drawn.
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Empty</dc:title></metadata>
<manifest>
  <item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
  <item id="pg" href="text/cover.xhtml" media-type="application/xhtml+xml"/>
</manifest>
<spine><itemref idref="c0"/></spine>
<guide><reference type="cover" href="text/cover.xhtml" title="Cover"/></guide>
</package>"#;
        let page = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><p>a page about the cover, with no picture on it</p></body></html>"#;
        let path = container(
            "pageless-cover",
            &[
                ("META-INF/container.xml", CONTAINER_XML),
                ("OEBPS/content.opf", opf),
                ("OEBPS/ch0.xhtml", CHAPTER),
                ("OEBPS/text/cover.xhtml", page),
            ],
        );
        let book = Book::open(&path).unwrap();
        assert_eq!(book.cover, None, "a page with no picture is no cover");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn one_entry_is_read_up_to_a_limit_and_no_further() {
        // A 438 KB book holding a 300 MB chapter took `inspect` to 2.6 GB
        // resident: whole entries were read with `read_to_end`. An entry past
        // the limit is refused — by name, so the message says which file did
        // it — instead of being believed.
        let big = "x".repeat(MAX_ENTRY_BYTES as usize + 1);
        let path = container(
            "entry-cap",
            &[
                ("META-INF/container.xml", CONTAINER_XML),
                ("OEBPS/content.opf", OPF),
                ("OEBPS/ch0.xhtml", CHAPTER),
                ("OEBPS/big.xhtml", &big),
            ],
        );
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let err = read_entry(&mut archive, "OEBPS/big.xhtml")
            .unwrap_err()
            .to_string();
        assert!(err.contains("OEBPS/big.xhtml"), "{err}");
        assert!(err.contains("32 MB"), "{err}");
        std::fs::remove_file(path).ok();
    }
}
