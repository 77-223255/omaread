//! Turns a chapter's XHTML into a flat list of blocks.
//!
//! EPUB requires chapter documents to be well-formed XML, so an XML parser
//! carries almost all of the load and we avoid pulling in a full HTML5 parser.
//! Measured across a large library, all but one chapter in a hundred parses; the
//! rest carry genuinely broken markup, such as `<strong><code></strong></code>`,
//! which no XML parser can accept. Those are reported to the caller.

use super::mathml;
use crate::doc::{Block, BlockKind, Link, RunBuilder, RunStyle};
use anyhow::{Context, Result};
use roxmltree::{Document, Node};
use std::borrow::Cow;

/// Elements whose content never reaches the reader.
const SKIPPED: &[&str] = &["head", "script", "style", "title", "template"];

/// Class names that mark a code listing.
///
/// `<pre>` is the tag for it, but few books use it. Pragmatic Bookshelf titles,
/// for one, wrap listings in `<div class="code">` and put every line in its own
/// `<p>`. Without recognising that, code arrives as prose: indentation collapsed
/// and a blank line between every line of it.
const CODE_CLASSES: &[&str] = &[
    "code",
    "codeblock",
    "listing",
    "programlisting",
    // Pragmatic Bookshelf builds listings as tables of this class, one row per
    // line, with a number cell beside the code cell.
    "processedcode",
    "sourcecode",
    "source-code",
    "highlight",
    "terminal",
    "console",
    "screen",
];

/// Cells that carry line numbers or gutter marks rather than code. Their content
/// is not part of the listing.
const CODE_GUTTER_CLASSES: &[&str] = &[
    "codeinfo",
    "codeprefix",
    "lineno",
    "linenos",
    "linenum",
    "linenumber",
    "gutter",
];

fn has_class(node: Node, names: &[&str]) -> bool {
    let Some(class) = node.attribute("class") else {
        return false;
    };
    let class = class.to_ascii_lowercase();
    class.split_whitespace().any(|name| names.contains(&name))
}

fn is_code_container(node: Node) -> bool {
    has_class(node, CODE_CLASSES)
}

/// What one chapter's markup yields.
pub struct Parsed {
    pub blocks: Vec<Block>,
    pub links: Vec<Link>,
    pub anchors: std::collections::HashMap<String, (usize, usize)>,
}

#[cfg(test)]
pub fn parse(xml: &str) -> Result<Vec<Block>> {
    Ok(parse_in(xml, "")?.blocks)
}

/// Parses a chapter that lives at `base` inside the container, so image paths
/// can be resolved relative to it.
pub fn parse_in(xml: &str, base: &str) -> Result<Parsed> {
    let cleaned = crate::epub::xml::clean(xml);
    let doc = Document::parse_with_options(&cleaned, crate::epub::xml::options())
        .context("chapter is not well-formed XML")?;

    let mut walker = Walker {
        base: base.to_string(),
        ..Walker::default()
    };
    let body = doc
        .descendants()
        .find(|n| n.is_element() && local_name(*n) == "body")
        .unwrap_or_else(|| doc.root_element());

    walker.walk_children(body, RunStyle::default());
    walker.flush();
    Ok(Parsed {
        blocks: walker.blocks,
        links: walker.links,
        anchors: walker.anchors,
    })
}

/// Block-level elements and the block kind they produce.
fn block_kind(name: &str) -> Option<BlockKind> {
    match name {
        "h1" => Some(BlockKind::Heading(1)),
        "h2" => Some(BlockKind::Heading(2)),
        "h3" => Some(BlockKind::Heading(3)),
        "h4" => Some(BlockKind::Heading(4)),
        "h5" => Some(BlockKind::Heading(5)),
        "h6" => Some(BlockKind::Heading(6)),
        "p" | "div" | "section" | "article" | "header" | "footer" | "figcaption" | "dd" | "dt"
        | "td" | "th" | "caption" => Some(BlockKind::Paragraph),
        "blockquote" => Some(BlockKind::Quote),
        "pre" => Some(BlockKind::Code),
        _ => None,
    }
}

fn inline_style(name: &str) -> Option<RunStyle> {
    let mut style = RunStyle::default();
    match name {
        "b" | "strong" => style.bold = true,
        "i" | "em" | "cite" | "dfn" | "var" => style.italic = true,
        "code" | "kbd" | "samp" | "tt" => style.code = true,
        "a" => style.link = true,
        _ => return None,
    }
    Some(style)
}

struct Walker {
    /// Directory of the chapter, for resolving image paths.
    base: String,
    blocks: Vec<Block>,
    current: RunBuilder,
    /// Kind the buffered text belongs to. A nested list must not turn the text
    /// of its enclosing list item into a paragraph, so the kind travels with the
    /// buffer instead of being passed at flush time.
    current_kind: BlockKind,
    /// Nesting depth of the enclosing lists, and whether each is ordered.
    lists: Vec<ListState>,
    /// Nesting depth of code containers. Inside one, whitespace is significant
    /// and every block becomes a code line.
    code_depth: usize,
    links: Vec<Link>,
    anchors: std::collections::HashMap<String, (usize, usize)>,
}

impl Default for Walker {
    fn default() -> Self {
        Self {
            base: String::new(),
            blocks: Vec::new(),
            current: RunBuilder::default(),
            current_kind: BlockKind::Paragraph,
            lists: Vec::new(),
            code_depth: 0,
            links: Vec::new(),
            anchors: std::collections::HashMap::new(),
        }
    }
}

struct ListState {
    ordered: bool,
    next_ordinal: usize,
}

impl Walker {
    /// Ends the current block, discarding it when it holds no text.
    fn flush(&mut self) {
        let builder = std::mem::take(&mut self.current);
        let kind = std::mem::replace(&mut self.current_kind, BlockKind::Paragraph);
        if builder.is_blank() {
            return;
        }
        // A code line keeps its leading spaces: that is its indentation.
        let runs = if kind == BlockKind::Code {
            builder.finish_verbatim()
        } else {
            builder.finish()
        };
        if runs.is_empty() {
            return;
        }
        self.blocks.push(Block { kind, runs });
    }

    /// Ends the current block and opens one of the given kind.
    fn open(&mut self, kind: BlockKind) {
        self.flush();
        self.current_kind = kind;
    }

    fn walk_children(&mut self, node: Node, style: RunStyle) {
        for child in node.children() {
            self.walk(child, style);
        }
    }

    fn walk(&mut self, node: Node, style: RunStyle) {
        if node.is_text() {
            if let Some(text) = node.text() {
                // Inside a listing, spaces carry meaning and must survive.
                let text = if self.code_depth > 0 {
                    strip_invisibles(text)
                } else {
                    collapse_whitespace(text)
                };
                self.current.push(&text, style);
            }
            return;
        }
        if !node.is_element() {
            return;
        }

        let name = local_name(node);
        if SKIPPED.contains(&name) {
            return;
        }

        // An id is a link target. Recorded before the element's content, so it
        // points at where that content begins.
        if let Some(id) = node.attribute("id") {
            let at = (self.blocks.len(), self.current.char_count());
            self.anchors.entry(id.to_string()).or_insert(at);
        }

        // Inside a listing, a number cell is not code and its content is dropped.
        if self.code_depth > 0 && has_class(node, CODE_GUTTER_CLASSES) {
            return;
        }

        // A listing container turns everything inside it into code.
        if self.code_depth == 0 && is_code_container(node) {
            self.flush();
            self.code_depth += 1;
            self.current_kind = BlockKind::Code;
            self.walk_children(node, style);
            self.flush();
            self.code_depth -= 1;
            return;
        }

        match name {
            "br" => {
                self.current.push(" ", style);
                return;
            }
            "hr" => {
                self.flush();
                self.blocks.push(Block {
                    kind: BlockKind::Rule,
                    runs: Vec::new(),
                });
                return;
            }
            "img" | "image" => {
                self.push_image(node);
                return;
            }
            "ul" | "ol" => {
                // Flushing keeps the enclosing list item's own text intact.
                self.flush();
                self.lists.push(ListState {
                    ordered: name == "ol",
                    next_ordinal: start_ordinal(node),
                });
                self.walk_children(node, style);
                self.lists.pop();
                return;
            }
            "li" => {
                self.push_list_item(node, style);
                return;
            }
            "pre" => {
                self.flush();
                self.push_preformatted(node, style);
                return;
            }
            "math" => {
                self.push_math(node, style);
                return;
            }
            "sub" | "sup" => {
                // A marker that carries a link is left to the normal walk, so
                // the link survives. Everything else is set as an index.
                if let Some(text) = mathml::script_text(node, name == "sub") {
                    self.current.push(&text, style);
                    return;
                }
            }
            _ => {}
        }

        if let Some(inline) = inline_style(name) {
            // A link is recorded with the range it covers, so the cursor can tell
            // whether it stands on one.
            if name == "a"
                && let Some(href) = node.attribute("href").filter(|h| !h.trim().is_empty()) {
                    let block = self.blocks.len();
                    let start = self.current.char_count();
                    self.walk_children(node, style.merged(inline));
                    let end = self.current.char_count();
                    // A link that spans a block boundary cannot be addressed as
                    // one range; its first part is enough to follow it.
                    if end > start && block == self.blocks.len() {
                        self.links.push(Link {
                            block,
                            start,
                            end,
                            target: href.trim().to_string(),
                        });
                    }
                    return;
                }
            self.walk_children(node, style.merged(inline));
            return;
        }

        match block_kind(name) {
            Some(kind) => {
                // Inside a listing every block is a line of code, whatever tag
                // the book used for it.
                let kind = if self.code_depth > 0 {
                    BlockKind::Code
                } else {
                    kind
                };
                self.open(kind);
                self.walk_children(node, style);
                self.flush();
            }
            // Unknown element: keep its text, do not open a block.
            None => self.walk_children(node, style),
        }
    }

    /// Writes a formula. A displayed one gets a block of its own, the way the
    /// book sets it off from the prose; an inline one joins the running text.
    fn push_math(&mut self, node: Node, style: RunStyle) {
        let text = mathml::render(node);
        if text.is_empty() {
            return;
        }
        // Inside a listing the formula is part of the code and must not break
        // the line it stands on.
        if node.attribute("display") == Some("block") && self.code_depth == 0 {
            self.open(BlockKind::Paragraph);
            self.current.push(&text, style);
            self.flush();
            return;
        }
        self.current.push(&text, style);
    }

    fn push_image(&mut self, node: Node) {
        let alt = node
            .attribute("alt")
            .map(str::trim)
            .filter(|a| !a.is_empty());
        // `xlink:href` covers SVG's `<image>`, which some books use.
        let src = node
            .attribute("src")
            .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")))
            .or_else(|| node.attribute("href"))
            .map(|src| super::container_path(&self.base, src));

        self.flush();
        let mut runs = RunBuilder::default();
        let label = alt.or(src.as_deref()).unwrap_or("image");
        runs.push(label, RunStyle::default());
        self.blocks.push(Block {
            kind: BlockKind::Image { src },
            runs: runs.finish(),
        });
    }

    fn push_list_item(&mut self, node: Node, style: RunStyle) {
        let depth = self.lists.len().saturating_sub(1) as u8;
        let ordinal = match self.lists.last_mut() {
            Some(list) if list.ordered => {
                let n = list.next_ordinal;
                list.next_ordinal += 1;
                Some(n)
            }
            _ => None,
        };
        self.open(BlockKind::ListItem { depth, ordinal });
        self.walk_children(node, style);
        self.flush();
    }

    /// Preformatted text keeps its line breaks, so each source line becomes its
    /// own block.
    fn push_preformatted(&mut self, node: Node, style: RunStyle) {
        let text = collect_raw_text(node);
        for line in text.lines() {
            let mut runs = RunBuilder::default();
            runs.push(
                line,
                style.merged(RunStyle {
                    code: true,
                    ..RunStyle::default()
                }),
            );
            let runs = runs.finish();
            self.blocks.push(Block {
                kind: BlockKind::Code,
                runs: if runs.is_empty() {
                    vec![crate::doc::Run {
                        text: String::new(),
                        style: RunStyle {
                            code: true,
                            ..RunStyle::default()
                        },
                    }]
                } else {
                    runs
                },
            });
        }
    }
}

fn local_name<'a>(node: Node<'a, 'a>) -> &'a str {
    node.tag_name().name()
}

fn start_ordinal(node: Node) -> usize {
    node.attribute("start")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1)
}

pub(super) fn collect_raw_text(node: Node) -> String {
    let mut out = String::new();
    for descendant in node.descendants() {
        if descendant.is_text()
            && let Some(text) = descendant.text() {
                out.push_str(text);
            }
    }
    out
}

/// Removes characters that take no space but break the layout.
///
/// Zero-width spaces are used as line anchors in some books. They are invisible,
/// yet they keep a block from counting as empty, which would leave a stray blank
/// line between every line of a listing.
///
/// Text holding none of them comes back as it stands, with no copy made.
fn strip_invisibles(text: &str) -> Cow<'_, str> {
    clean(text, false)
}

/// Collapses runs of whitespace into single spaces, as HTML rendering does.
///
/// Only ASCII whitespace collapses. A no-break space is content: it must stay a
/// distinct character so the line breaker does not split there.
///
/// Most text nodes are already clean — nothing to strip, no run to collapse —
/// and those come back borrowed; copying them twice apiece was the whole cost
/// of cleaning a chapter.
pub(super) fn collapse_whitespace(text: &str) -> Cow<'_, str> {
    clean(text, true)
}

/// The one pass behind both: marks that never show are dropped, and — when
/// `collapse` — runs of ASCII whitespace become single spaces. The input is
/// returned as it stands for as long as the output would be byte-for-byte the
/// same, so the copy starts only at the first character that differs.
fn clean(text: &str, collapse: bool) -> Cow<'_, str> {
    let mut out: Option<String> = None;
    // Whether what has been written so far ends in the space a run collapsed to.
    let mut in_space = false;
    for (at, ch) in text.char_indices() {
        if matches!(ch, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\r') {
            // Nothing before this mark needs rewording, so the copy starts with
            // the text exactly as written up to here — and the mark is left out.
            out.get_or_insert_with(|| text[..at].to_owned());
            continue;
        }
        if collapse && ch.is_ascii_whitespace() {
            if in_space {
                // Second character of a run: the run's one space already stands,
                // so this one drops out of the output.
                out.get_or_insert_with(|| text[..at].to_owned());
                continue;
            }
            if ch == ' ' && out.is_none() {
                // A lone space reads the same collapsed: nothing has changed,
                // and whether it ever will is for the next character to say.
                in_space = true;
                continue;
            }
            out.get_or_insert_with(|| text[..at].to_owned()).push(' ');
            in_space = true;
            continue;
        }
        if let Some(written) = out.as_mut() {
            written.push(ch);
        }
        in_space = false;
    }
    match out {
        Some(written) => Cow::Owned(written),
        None => Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_headings_and_paragraphs() {
        // Style and script are never book text: only what is left after them
        // counts.
        let blocks = parse(
            r#"<html><head><style>p{color:red}</style></head>
               <body><h1>Title</h1><p>First <em>word</em>.</p><script>x=1</script></body></html>"#,
        )
        .unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(blocks[0].plain_text(), "Title");
        assert_eq!(blocks[1].plain_text(), "First word.");
        assert!(blocks[1].runs.iter().any(|r| r.style.italic));
    }

    #[test]
    fn list_items_keep_their_number_and_their_nesting() {
        let blocks = parse(r#"<html><body><ol><li>a</li><li>b</li></ol></body></html>"#).unwrap();
        assert_eq!(
            blocks[0].kind,
            BlockKind::ListItem {
                depth: 0,
                ordinal: Some(1)
            }
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::ListItem {
                depth: 0,
                ordinal: Some(2)
            }
        );

        // The outer item is kept beside the nested one, and the outer list
        // keeps counting after it.
        let blocks = parse(
            r#"<html><body><ol>
                 <li>outer one<ol><li>inner</li></ol></li>
                 <li>outer two</li>
               </ol></body></html>"#,
        )
        .unwrap();
        let texts: Vec<String> = blocks.iter().map(|b| b.plain_text()).collect();
        assert_eq!(texts, vec!["outer one", "inner", "outer two"]);
        assert_eq!(
            blocks[0].kind,
            BlockKind::ListItem {
                depth: 0,
                ordinal: Some(1)
            }
        );
        assert_eq!(
            blocks[1].kind,
            BlockKind::ListItem {
                depth: 1,
                ordinal: Some(1)
            }
        );
        assert_eq!(
            blocks[2].kind,
            BlockKind::ListItem {
                depth: 0,
                ordinal: Some(2)
            }
        );
    }

    #[test]
    fn text_comes_out_as_it_reads() {
        // Whitespace between the lines of one sentence is one space, and an
        // entity nobody declared is the character it stands for.
        let blocks = parse("<html><body><p>one\n  two\t three</p></body></html>").unwrap();
        assert_eq!(blocks[0].plain_text(), "one two three");
        let blocks = parse("<html><body><p>a&nbsp;b &mdash; c</p></body></html>").unwrap();
        assert_eq!(blocks[0].plain_text(), "a\u{00a0}b \u{2014} c");
    }

    #[test]
    fn cleaning_changes_only_what_it_always_changed() {
        // Where the text changes, it changes exactly what it always did: runs
        // collapse to one space, marks and carriage returns go.
        assert_eq!(&*collapse_whitespace("a  b\tc\nd"), "a b c d");
        assert_eq!(&*collapse_whitespace(" \u{200b}\r\nnext"), " next");
        assert_eq!(&*strip_invisibles("one\r\ntwo\u{feff}"), "one\ntwo");
        // A no-break space is content, not whitespace to collapse.
        assert_eq!(&*collapse_whitespace("a\u{00a0}  b"), "a\u{00a0} b");
    }

    #[test]
    fn a_percent_encoded_picture_name_is_decoded() {
        // The container holds `海报.png`; the chapter names it the way a URI does.
        let parsed = parse_in(
            r#"<html><body><img src="Images/%E6%B5%B7%E6%8A%A5.png" alt="poster"/></body></html>"#,
            "OEBPS",
        )
        .unwrap();
        match &parsed.blocks[0].kind {
            crate::doc::BlockKind::Image { src } => {
                assert_eq!(src.as_deref(), Some("OEBPS/Images/海报.png"));
            }
            other => panic!("expected a picture, got {other:?}"),
        }
    }

    #[test]
    fn keeps_line_breaks_in_preformatted_text() {
        let blocks = parse("<html><body><pre>one\ntwo</pre></body></html>").unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, BlockKind::Code);
        assert_eq!(blocks[1].plain_text(), "two");
    }

    #[test]
    fn a_formula_comes_out_as_text_where_it_was_written() {
        // A displayed formula is a block of its own, an inline one stays in
        // the running text, and an index set from prose comes out as the
        // character it is — all as text, in the place they were written.
        let blocks = parse(
            r#"<html><body><p>before</p><div class="disp-formulau">
               <math xmlns="http://www.w3.org/1998/Math/MathML" display="block">
                 <msub><mi>R</mi><mi>s</mi></msub><mo>=</mo><msub><mi>R</mi><mn>1</mn></msub>
               </math></div><p>after</p></body></html>"#,
        )
        .unwrap();
        let text: Vec<String> = blocks.iter().map(|b| b.plain_text()).collect();
        assert_eq!(text, vec!["before", "Rₛ = R₁", "after"]);

        let blocks = parse(
            r#"<html><body><p>a <math xmlns="http://www.w3.org/1998/Math/MathML" display="inline"><mfrac><mn>5</mn><mn>16</mn></mfrac></math> bolt</p></body></html>"#,
        )
        .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].plain_text(), "a 5/16 bolt");

        let blocks =
            parse("<html><body><p>R<sub>s</sub> and x<sup>2</sup></p></body></html>").unwrap();
        assert_eq!(blocks[0].plain_text(), "Rₛ and x²");
    }

    #[test]
    fn leaves_a_footnote_marker_a_link() {
        let parsed = parse_in(
            r#"<html><body><p>text<sup><a href="notes.xhtml#n1">1</a></sup></p></body></html>"#,
            "",
        )
        .unwrap();
        assert_eq!(parsed.blocks[0].plain_text(), "text1");
        assert_eq!(parsed.links.len(), 1);
        assert_eq!(parsed.links[0].target, "notes.xhtml#n1");
    }
}
