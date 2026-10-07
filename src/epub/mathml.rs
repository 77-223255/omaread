//! Turns a MathML subtree into one line of text.
//!
//! A chapter is a flat list of blocks of styled runs, see `doc.rs`. Nothing in
//! that model can stack one expression over another, so a formula has to arrive
//! as a single line. Indices become Unicode sub- and superscripts wherever every
//! character of the index has one, and fall back to `_x` and `^(x)` otherwise.
//! That is the notation a reader already knows from plain-text mathematics, and
//! it never hides which part is the index.
//!
//! Subscripts exist for lowercase letters only, so superscripts are limited to
//! the same range. Otherwise `Q^A` would be set as `Qᴬ` while `Q_A` fell back to
//! `Q_A`, and the two would no longer look like a pair.

use super::xhtml::{collapse_whitespace, collect_raw_text};
use roxmltree::Node;

/// Renders a `<math>` element into one line. Empty when the element holds
/// nothing that can be shown.
pub fn render(node: Node) -> String {
    let mut out = Out::new(false);
    render_children(node, &mut out);
    out.finish()
}

/// Text for an HTML `<sub>` or `<sup>`, if the element can be set as one.
///
/// Returns `None` when the element holds markup rather than plain text. A
/// footnote marker is written `<sup><a href="...">1</a></sup>`; converting it
/// would drop the link, so such an element is left to the normal walk.
pub fn script_text(node: Node, sub: bool) -> Option<String> {
    if node.children().any(|child| child.is_element()) {
        return None;
    }
    let raw = collapse_whitespace(&collect_raw_text(node));
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    Some(if sub {
        to_subscript(text).unwrap_or_else(|| fallback('_', text))
    } else {
        to_superscript(text).unwrap_or_else(|| fallback('^', text))
    })
}

/// Collects the rendered text, inserting the spacing a typesetter would set
/// around an operator.
struct Out {
    text: String,
    /// Inside an index every space is dropped: `∏` carries `i=1`, not `i = 1`.
    tight: bool,
    /// A space is owed before whatever comes next. Held back so that a trailing
    /// space never reaches the output.
    pending: bool,
    /// True while the last thing written was an operator or an opening fence.
    /// A `+` or `−` in that position is a sign, not an operation, and is set
    /// without spaces.
    after_operator: bool,
}

impl Out {
    fn new(tight: bool) -> Self {
        Self {
            text: String::new(),
            tight,
            pending: false,
            after_operator: true,
        }
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.pending && !self.text.is_empty() {
            self.text.push(' ');
        }
        self.pending = false;
        self.after_operator = false;
        self.text.push_str(text);
    }

    fn space(&mut self) {
        if !self.tight && !self.text.is_empty() {
            self.pending = true;
        }
    }

    /// Takes back a space that is owed. A comma and a closing fence bind to
    /// what stands before them, even when that was a spaced operator: an
    /// enumeration must read `P₁, P₂, ⋯, Pₘ`, not `P₁, P₂, ⋯ , Pₘ`.
    fn drop_space(&mut self) {
        self.pending = false;
    }

    fn finish(self) -> String {
        self.text.trim().to_string()
    }
}

fn element_children<'a>(node: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    node.children().filter(|child| child.is_element()).collect()
}

fn render_children(node: Node, out: &mut Out) {
    for child in node.children() {
        render_node(child, out);
    }
}

/// Renders one node on its own, with the surrounding spacing rules still in
/// force.
fn render_alone(node: Node) -> String {
    let mut out = Out::new(false);
    render_node(node, &mut out);
    out.finish()
}

/// Renders one node as an index, where spaces are dropped.
fn render_tight(node: Node) -> String {
    let mut out = Out::new(true);
    render_node(node, &mut out);
    out.finish()
}

fn render_node(node: Node, out: &mut Out) {
    if node.is_text() {
        // In MathML only a token element carries text. Whitespace between
        // elements is indentation and means nothing, unlike in HTML. O'Reilly
        // titles set every element on its own line; taking that as content
        // turns `<mi>l</mi><mi>o</mi><mi>g</mi>` into `l o g`. Real space comes
        // from `<mspace/>` or from `<mo>`.
        if let Some(text) = node.text().filter(|text| !text.trim().is_empty()) {
            push_text(text, out);
        }
        return;
    }
    if !node.is_element() {
        return;
    }

    let name = node.tag_name().name();
    let kids = element_children(node);
    match name {
        // Grouping and styling elements carry no meaning of their own. The
        // book's `mathvariant` and `mathsize` are dropped: a run has no place
        // to keep them.
        "math" | "mrow" | "mstyle" | "mpadded" | "menclose" | "merror" | "mtd" => {
            render_children(node, out)
        }
        // Only the presentation branch of an annotated expression is shown.
        "semantics" => {
            if let Some(first) = kids.first() {
                render_node(*first, out);
            }
        }
        "annotation" | "annotation-xml" => {}
        // A phantom reserves space without showing anything.
        "mphantom" | "mspace" => out.space(),
        "mi" | "mn" | "ms" | "mtext" => push_text(&collect_raw_text(node), out),
        "mo" => push_operator(&collect_raw_text(node), out),
        "msub" | "munder" => push_scripted(&kids, Some(1), None, out),
        "msup" | "mover" => push_scripted(&kids, None, Some(1), out),
        "msubsup" | "munderover" => push_scripted(&kids, Some(1), Some(2), out),
        "mfrac" => {
            let numerator = kids.first().map(|n| render_alone(*n)).unwrap_or_default();
            let denominator = kids.get(1).map(|n| render_alone(*n)).unwrap_or_default();
            out.push(&format!("{}/{}", group(&numerator), group(&denominator)));
        }
        "msqrt" => {
            let mut inner = Out::new(false);
            render_children(node, &mut inner);
            out.push(&format!("√{}", group(&inner.finish())));
        }
        "mroot" => {
            let radicand = kids.first().map(|n| render_alone(*n)).unwrap_or_default();
            let degree = kids.get(1).map(|n| render_tight(*n)).unwrap_or_default();
            let mark = match to_superscript(&degree) {
                Some(script) => format!("{script}√"),
                None => format!("√[{degree}]"),
            };
            out.push(&format!("{mark}{}", group(&radicand)));
        }
        // A table is the one construct that truly needs more than a line. Its
        // rows are separated by a semicolon, which at least keeps them apart.
        "mtable" => {
            let rows: Vec<String> = kids
                .iter()
                .filter(|row| matches!(row.tag_name().name(), "mtr" | "mlabeledtr"))
                .map(|row| {
                    element_children(*row)
                        .iter()
                        .map(|cell| render_alone(*cell))
                        .filter(|cell| !cell.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|row| !row.is_empty())
                .collect();
            out.push(&rows.join("; "));
        }
        _ => render_children(node, out),
    }
}

/// Writes a base with an index below, above, or both. `sub` and `sup` name the
/// positions of the two indices among the element's children.
fn push_scripted(kids: &[Node], sub: Option<usize>, sup: Option<usize>, out: &mut Out) {
    // A base can be empty: books attach an exponent to an already subscripted
    // symbol by writing `<msup><mrow/><mrow>n1</mrow></msup>` after it.
    let base = kids.first().map(|n| render_alone(*n)).unwrap_or_default();
    let below = sub
        .and_then(|at| kids.get(at))
        .map(|n| render_tight(*n))
        .unwrap_or_default();
    let above = sup
        .and_then(|at| kids.get(at))
        .map(|n| render_tight(*n))
        .unwrap_or_default();

    let (below, above) = match (below.is_empty(), above.is_empty()) {
        (true, true) => (String::new(), String::new()),
        (false, true) => (
            to_subscript(&below).unwrap_or_else(|| fallback('_', &below)),
            String::new(),
        ),
        (true, false) => (
            String::new(),
            to_superscript(&above).unwrap_or_else(|| fallback('^', &above)),
        ),
        // Both indices belong to one symbol. If either has to fall back, both
        // do, so that the pair keeps one notation. Parentheses are set even
        // around a single character here: the upper index of `∏_(i=1)^(n)` is
        // followed by the term itself, and `^n` would run into it.
        (false, false) => match (to_subscript(&below), to_superscript(&above)) {
            (Some(low), Some(high)) => (low, high),
            _ => (format!("_({below})"), format!("^({above})")),
        },
    };
    out.push(&format!("{base}{below}{above}"));
}

fn push_text(text: &str, out: &mut Out) {
    let text = collapse_whitespace(text);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        if !text.is_empty() {
            out.space();
        }
        return;
    }
    if text.starts_with(' ') {
        out.space();
    }
    out.push(trimmed);
    if text.ends_with(' ') {
        out.space();
    }
}

fn push_operator(text: &str, out: &mut Out) {
    let collapsed = collapse_whitespace(text);
    // An operator built only from invisible marks shows nothing, not even the
    // space that an empty operator would stand for.
    if collapsed.chars().any(is_invisible) && collapsed.chars().all(|c| is_invisible(c) || c == ' ')
    {
        return;
    }
    let visible: String = collapsed.chars().filter(|c| !is_invisible(*c)).collect();
    let op = visible.trim();
    if op.is_empty() {
        // An operator that is nothing but space is used as a separator.
        if !collapsed.is_empty() {
            out.space();
        }
        return;
    }
    if is_sign(op) && out.after_operator {
        out.push(op);
        out.after_operator = true;
        return;
    }
    if is_spaced(op) {
        out.space();
        out.push(op);
        out.space();
        out.after_operator = true;
        return;
    }
    if is_open_fence(op) {
        out.push(op);
        out.after_operator = true;
        return;
    }
    if op == "," || op == ";" {
        out.drop_space();
        out.push(op);
        out.space();
        out.after_operator = true;
        return;
    }
    // Closing fences, dots and primes bind to what stands before them.
    out.drop_space();
    out.push(op);
}

/// Characters that occupy no space and mark structure a reader cannot see.
fn is_invisible(ch: char) -> bool {
    matches!(ch, '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}')
}

fn is_sign(op: &str) -> bool {
    matches!(op, "+" | "-" | "−" | "±" | "∓")
}

/// Operators that read as a relation or a binary operation, and a run of dots
/// standing for the omitted middle of a sequence. Each gets a space on both
/// sides.
fn is_spaced(op: &str) -> bool {
    const SPACED: &[&str] = &[
        "=", "≠", "<", ">", "≤", "≥", "≈", "≡", "≅", "∼", "∝", "→", "←", "↔", "⇒", "⇐", "⇔", "↦",
        "≪", "≫", "+", "-", "−", "±", "∓", "×", "÷", "⋅", "·", "∗", "∘", "∪", "∩", "∈", "∉", "⊂",
        "⊃", "⊆", "⊇", "∧", "∨", "*", "…", "⋯", "⋮", "⋱", "⋰",
    ];
    if SPACED.contains(&op) {
        return true;
    }
    // Named operators such as `mod`, `max` or `lim` are words and need air.
    op.chars().count() > 1 && op.chars().all(|c| c.is_alphabetic())
}

fn is_open_fence(op: &str) -> bool {
    matches!(op, "(" | "[" | "{" | "⌈" | "⌊" | "⟨")
}

/// Wraps an expression in parentheses unless it is a single symbol already, or
/// already carries its own.
fn group(text: &str) -> String {
    if text.is_empty() || is_atomic(text) || is_wrapped(text) {
        return text.to_string();
    }
    format!("({text})")
}

fn is_atomic(text: &str) -> bool {
    text.chars().all(|c| c.is_alphanumeric() || c == '.')
}

fn is_wrapped(text: &str) -> bool {
    let mut chars = text.chars();
    if chars.next() != Some('(') || !text.ends_with(')') {
        return false;
    }
    // The opening parenthesis must be the one that the last character closes,
    // otherwise `(a)/(b)` would count as wrapped.
    let mut depth = 1usize;
    let last = text.chars().count() - 1;
    for (at, ch) in chars.enumerate() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return at + 1 == last;
                }
            }
            _ => {}
        }
    }
    false
}

/// Notation for an index that has no Unicode form. A single character needs no
/// parentheses; anything longer does, or its end would be unreadable.
fn fallback(marker: char, text: &str) -> String {
    if text.chars().count() == 1 {
        format!("{marker}{text}")
    } else {
        format!("{marker}({text})")
    }
}

fn to_superscript(text: &str) -> Option<String> {
    text.chars()
        .map(|ch| superscript_char(ch).or_else(|| keeps_its_shape(ch)))
        .collect()
}

fn to_subscript(text: &str) -> Option<String> {
    text.chars()
        .map(|ch| subscript_char(ch).or_else(|| keeps_its_shape(ch)))
        .collect()
}

/// A symbol that has no raised or lowered form is written as it stands.
///
/// `<sup>®</sup>` is the common case: the font sets the sign high already, and
/// `GDPR For Dummies^®` would be worse than `GDPR For Dummies®`. Letters and
/// digits are excluded, because for those the marker is the only thing that
/// says they are an index. So is whitespace, which would make a footnote read
/// as part of the sentence.
fn keeps_its_shape(ch: char) -> Option<char> {
    (!ch.is_alphanumeric() && !ch.is_whitespace()).then_some(ch)
}

fn superscript_char(ch: char) -> Option<char> {
    Some(match ch {
        '0'..='9' => "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().nth(ch as usize - '0' as usize)?,
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        _ => return None,
    })
}

fn subscript_char(ch: char) -> Option<char> {
    Some(match ch {
        '0'..='9' => "₀₁₂₃₄₅₆₇₈₉".chars().nth(ch as usize - '0' as usize)?,
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use roxmltree::Document;

    /// Renders a `<math>` element given as a string. The samples are taken from
    /// real books, with the presentational attributes removed.
    fn math(xml: &str) -> String {
        let doc = Document::parse(xml).unwrap();
        render(doc.root_element())
    }

    #[test]
    fn indices_use_unicode_where_there_is_one_and_plain_where_there_is_not() {
        assert_eq!(math("<math><msub><mi>R</mi><mi>s</mi></msub></math>"), "Rₛ");
        assert_eq!(
            math(
                "<math><msup><mrow><mo>(</mo><mn>0.99</mn><mo>)</mo></mrow><mn>25</mn></msup></math>"
            ),
            "(0.99)²⁵"
        );
        // Lambda has no superscript, so the whole exponent is written out —
        // and so is a subscript with no Unicode form of its own.
        assert_eq!(
            math("<math><msup><mi>e</mi><mrow><mo>−</mo><mi>λ</mi><mi>t</mi></mrow></msup></math>"),
            "e^(−λt)"
        );
        assert_eq!(math("<math><msub><mi>Q</mi><mi>P</mi></msub></math>"), "Q_P");
        // Books write `Q_a1^n1` as a subscripted symbol followed by an
        // exponent whose base is an empty row.
        let xml = "<math><msub><mi>Q</mi><mrow><mi>a</mi><mn>1</mn></mrow></msub>\
                   <msup><mrow/><mrow><mi>n</mi><mn>1</mn></mrow></msup></math>";
        assert_eq!(math(xml), "Qₐ₁ⁿ¹");
        // A product's limits stay apart — the lower one written out where it
        // has no subscript of its own.
        let xml = "<math><munderover><mi>Π</mi><mrow><mi>I</mi><mo>=</mo><mn>1</mn></mrow>                   <mi>n</mi></munderover><msub><mi>R</mi><mi>i</mi></msub></math>";
        assert_eq!(math(xml), "Π_(I=1)^(n)Rᵢ");
    }

    #[test]
    fn spacing_follows_the_symbol() {
        // A relation is spaced, a sign is not, and a fence or a decimal point
        // binds tight to what it is set against.
        let xml = "<math><mi>a</mi><mo>=</mo><mo>−</mo><mi>b</mi></math>";
        assert_eq!(math(xml), "a = −b");
        let xml = "<math><mo>(</mo><mn>0</mn><mo>.</mo><mn>9</mn><mn>9</mn><mo>)</mo></math>";
        assert_eq!(math(xml), "(0.99)");
        // A comma binds to what stands before it.
        let xml = "<math><msub><mi>P</mi><mn>1</mn></msub><mo>,</mo>\
                   <mo>⋯</mo><mo>,</mo><msub><mi>P</mi><mi>m</mi></msub></math>";
        assert_eq!(math(xml), "P₁, ⋯, Pₘ");
        // And a space the markup asks for is a space, not a collapsed one.
        let xml =
            "<math><mtext>Coffee</mtext><mspace width=\"4.pt\"/><mtext>Drinker</mtext></math>";
        assert_eq!(math(xml), "Coffee Drinker");
    }

    #[test]
    fn a_two_dimensional_structure_comes_out_on_one_line() {
        let simple =
            "<math><mfrac bevelled=\"true\"><mn>5</mn><mrow><mn>16</mn></mrow></mfrac></math>";
        assert_eq!(math(simple), "5/16");
        let compound = "<math><mfrac><mtext>Uptime</mtext>\
                        <mrow><mtext>Uptime</mtext><mo>+</mo><mtext>Downtime</mtext></mrow></mfrac></math>";
        assert_eq!(math(compound), "Uptime/(Uptime + Downtime)");

        assert_eq!(math("<math><msqrt><mn>2</mn></msqrt></math>"), "√2");
        let sum = "<math><msqrt><mi>a</mi><mo>+</mo><mi>b</mi></msqrt></math>";
        assert_eq!(math(sum), "√(a + b)");
        let cube = "<math><mroot><mi>x</mi><mn>3</mn></mroot></math>";
        assert_eq!(math(cube), "³√x");
    }

    #[test]
    fn a_formula_comes_out_as_its_visible_characters_only() {
        // The semantic annotation is a copy of the formula in another
        // language, not part of it, and U+2062 (invisible times) marks
        // structure without showing it.
        let xml = "<math><semantics><mrow><mi>x</mi></mrow>\
                   <annotation encoding=\"application/x-tex\">x</annotation></semantics></math>";
        assert_eq!(math(xml), "x");
        let xml = "<math><mi>a</mi><mo>\u{2062}</mo><mi>b</mi></math>";
        assert_eq!(math(xml), "ab");

        // O'Reilly titles set every element on its own line. Taking that as
        // content would spell the logarithm `l o g`.
        let xml = "<math>\n  <mi>l</mi>\n  <mi>o</mi>\n  <mi>g</mi>\n  <mn>2</mn>\n</math>";
        assert_eq!(math(xml), "log2");
    }

    #[test]
    fn the_rows_of_a_table_are_separated() {
        let xml = "<math><mtable><mtr><mtd><mi>a</mi></mtd><mtd><mn>1</mn></mtd></mtr>\
                   <mtr><mtd><mi>b</mi></mtd><mtd><mn>2</mn></mtd></mtr></mtable></math>";
        assert_eq!(math(xml), "a 1; b 2");
    }

    #[test]
    fn an_html_index_is_converted_unless_it_carries_markup() {
        let doc = Document::parse("<sub>2</sub>").unwrap();
        assert_eq!(script_text(doc.root_element(), true).as_deref(), Some("₂"));
        let doc = Document::parse("<sup>3</sup>").unwrap();
        assert_eq!(script_text(doc.root_element(), false).as_deref(), Some("³"));
        let doc = Document::parse("<sub>max</sub>").unwrap();
        assert_eq!(
            script_text(doc.root_element(), true).as_deref(),
            Some("ₘₐₓ")
        );
        // A registered sign is set high by the font already, and a footnote
        // marker that continues an earlier one keeps its comma.
        let doc = Document::parse("<sup>®</sup>").unwrap();
        assert_eq!(script_text(doc.root_element(), false).as_deref(), Some("®"));
        let doc = Document::parse("<sup>,11</sup>").unwrap();
        assert_eq!(
            script_text(doc.root_element(), false).as_deref(),
            Some(",¹¹")
        );
        // But a marker with a link inside it is left alone: converting it
        // would drop the link.
        let xml = "<sup><a href=\"note.xhtml\">1</a></sup>";
        let doc = Document::parse(xml).unwrap();
        assert_eq!(script_text(doc.root_element(), false), None);
    }
}
