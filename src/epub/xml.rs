//! The one way into XML for everything in this module tree.
//!
//! Chapter XHTML, the container, the package document, the navigation and the
//! NCX are all XML, and each needs the same two things before roxmltree will
//! read it: HTML's named entities resolved first — roxmltree knows only the
//! five XML defines — and DTDs allowed, because a doctype is legal and the
//! parser refuses one by default. Handing one without the other is how a book
//! silently loses the em dash in its title, or its title altogether.

use roxmltree::ParsingOptions;
use std::borrow::Cow;

/// Chapter documents carry a doctype and, in EPUB 2, an internal entity subset.
/// Both are refused by default, so parsing must allow DTDs explicitly.
pub(crate) fn options<'input>() -> ParsingOptions<'input> {
    ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    }
}

/// Replaces named HTML entities that XML does not define. Entities declared in
/// a document's own internal subset are left to the parser; this only covers
/// the common ones that EPUB files use without declaring them.
///
/// A document with no `&` — the usual one — is handed back untouched, and one
/// with `&` but none of these entities is resolved in a single pass, instead of
/// one scan and one rebuilt copy per entity.
///
/// The five entities XML defines itself — `&amp;`, `&lt;`, `&gt;`, `&quot;`,
/// `&apos;` — must never appear in the table below: roxmltree resolves those on
/// its own, and pre-resolving `&amp;` would hand it a bare `&` to read as the
/// start of some other entity, which no parse survives. So the table holds only
/// what the parser cannot know, and an ampersand it does know stands as written.
pub(crate) fn clean(xml: &str) -> Cow<'_, str> {
    if !xml.contains('&') {
        return Cow::Borrowed(xml);
    }
    let mut out: Option<String> = None;
    // How much of the input the copy already holds; only once it exists.
    let mut copied = 0;
    let mut at = 0;
    while at < xml.len() {
        // Only an ASCII `&` can start an entity, so this byte is a character
        // boundary and the slice from here is safe.
        if xml.as_bytes()[at] == b'&'
            && let Some((entity, replacement)) = ENTITIES
                .iter()
                .find(|(name, _)| xml[at..].starts_with(*name))
        {
            let written = out.get_or_insert_with(|| String::with_capacity(xml.len()));
            written.push_str(&xml[copied..at]);
            written.push_str(replacement);
            copied = at + entity.len();
            at = copied;
            continue;
        }
        at += 1;
    }
    match out {
        // An `&` the parser owns, or none at all: nothing was resolved.
        None => Cow::Borrowed(xml),
        Some(mut written) => {
            written.push_str(&xml[copied..]);
            Cow::Owned(written)
        }
    }
}

/// Named entities that appear in EPUB content but are not predefined in XML.
/// `&amp;`, `&lt;`, `&gt;`, `&quot;` and `&apos;` are left to the parser.
///
/// Scanned in order rather than searched: nothing here is a prefix of anything
/// else, so the first name that fits is the only one that can.
const ENTITIES: &[(&str, &str)] = &[
    ("&nbsp;", "\u{00a0}"),
    ("&ndash;", "\u{2013}"),
    ("&mdash;", "\u{2014}"),
    ("&lsquo;", "\u{2018}"),
    ("&rsquo;", "\u{2019}"),
    ("&ldquo;", "\u{201c}"),
    ("&rdquo;", "\u{201d}"),
    ("&hellip;", "\u{2026}"),
    ("&copy;", "\u{00a9}"),
    ("&reg;", "\u{00ae}"),
    ("&trade;", "\u{2122}"),
    ("&deg;", "\u{00b0}"),
    ("&middot;", "\u{00b7}"),
    ("&bull;", "\u{2022}"),
    ("&dagger;", "\u{2020}"),
    ("&eacute;", "\u{00e9}"),
    ("&egrave;", "\u{00e8}"),
    ("&auml;", "\u{00e4}"),
    ("&ouml;", "\u{00f6}"),
    ("&uuml;", "\u{00fc}"),
    ("&Auml;", "\u{00c4}"),
    ("&Ouml;", "\u{00d6}"),
    ("&Uuml;", "\u{00dc}"),
    ("&szlig;", "\u{00df}"),
    ("&euro;", "\u{20ac}"),
    ("&pound;", "\u{00a3}"),
    ("&times;", "\u{00d7}"),
    ("&frac12;", "\u{00bd}"),
    ("&thinsp;", "\u{2009}"),
    ("&shy;", "\u{00ad}"),
    ("&ensp;", "\u{2002}"),
    ("&emsp;", "\u{2003}"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entity_is_resolved_once_and_only_where_one_is() {
        // The fast path: a document without an ampersand is neither scanned for
        // entities nor copied.
        assert!(matches!(
            clean("plain words, nothing to resolve"),
            Cow::Borrowed(_)
        ));
        // XML's own five belong to the parser, so a document using only them is
        // left standing as well — and so is one whose ampersands name nothing
        // this reader knows.
        assert!(matches!(
            clean("a &amp; b &lt; c &gt; d &quot; e &apos; f"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(clean("x &nosuch; y"), Cow::Borrowed(_)));
        // Whatever is resolved comes out resolved, wherever it stands, as it
        // would have after thirty-two passes over the document.
        assert_eq!(
            &*clean("a &mdash; b&nbsp;c &mdash; d"),
            "a \u{2014} b\u{00a0}c \u{2014} d"
        );
    }
}
