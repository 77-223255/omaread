//! Finding out what the terminal can draw.
//!
//! The user picks the terminal, not us, so the reader asks instead of assuming.
//! Two questions go out and the answers decide: the Kitty graphics query and the
//! primary device attributes, whose answer lists Sixel support as feature 4.
//!
//! Anything unanswered means no. A terminal that stays silent must not hold up
//! the start, so the wait is short and failure is ordinary — the window these
//! questions are asked in belongs to the session, which asks its own questions
//! (the colours to draw with) in the same one.

/// The question: a one-pixel Kitty image query, so a terminal that
/// misunderstands it still paints nothing, and a primary device attributes
/// request that every terminal answers — that answer marks the end of the
/// replies, so the wait need not run out before the reply is classified.
pub const ASK: &str = concat!("\x1b_Gi=1,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\", "\x1b[c");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// A block per cell: four quadrants, two colours, drawn as ordinary text.
    /// Works in every terminal and through tmux.
    Quad,
    /// Real pixels, supported by foot among others.
    Sixel,
    /// Real pixels, supported by Ghostty and kitty.
    Kitty,
}

impl Backend {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "quad" | "quadrant" => Some(Backend::Quad),
            "sixel" => Some(Backend::Sixel),
            "kitty" => Some(Backend::Kitty),
            _ => None,
        }
    }
}

/// Whether a pixel protocol could work here at all.
///
/// Inside a multiplexer the pixel protocols are out of reach. tmux manages the
/// screen itself and knows nothing about the pixels a passthrough would put
/// there, so a picture would survive scrolling and cover the text. Blocks are
/// made of ordinary cells and behave.
pub fn pixels_possible() -> bool {
    std::env::var_os("TMUX").is_none() && !is_screen()
}

fn is_screen() -> bool {
    std::env::var("TERM")
        .map(|term| term.starts_with("screen"))
        .unwrap_or(false)
}

/// Reads a terminal's answers. Kitty support outranks Sixel, because it carries
/// full colour while Sixel is limited to a palette.
pub fn classify(reply: &str) -> Backend {
    if reply.contains("_Gi=1;OK") || reply.contains("_Gi=1;ok") {
        return Backend::Kitty;
    }
    if supports_sixel(reply) {
        return Backend::Sixel;
    }
    Backend::Quad
}

/// Looks for feature 4 in a primary device attributes answer, which is Sixel.
///
/// The answer looks like `ESC [ ? 62 ; 4 ; 22 c`, so the features are the
/// semicolon-separated numbers between `?` and `c`.
fn supports_sixel(reply: &str) -> bool {
    let Some(start) = reply.find("\x1b[?") else {
        return false;
    };
    let rest = &reply[start + 3..];
    let Some(end) = rest.find('c') else {
        return false;
    };
    rest[..end].split(';').any(|feature| feature.trim() == "4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_what_picks_the_backend() {
        // What a terminal may answer, and the backend a reader gets for it:
        // kitty outranks sixel when it offers both, an answer without either
        // feature leaves the blocks, and silence is the same as saying
        // nothing at all.
        let answers: &[(&str, Backend)] = &[
            ("\x1b_Gi=1;OK\x1b\\\x1b[?62;c", Backend::Kitty),
            ("\x1b_Gi=1;OK\x1b\\\x1b[?62;4;22c", Backend::Kitty),
            ("\x1b[?62;4;22c", Backend::Sixel),
            ("\x1b[?6c", Backend::Quad),
            ("\x1b[?62;22c", Backend::Quad),
            ("", Backend::Quad),
            ("nonsense", Backend::Quad),
        ];
        for (reply, want) in answers {
            assert_eq!(classify(reply), *want, "{reply:?}");
        }

        // Four is the Sixel feature — and forty, and fourteen, are not it.
        assert!(supports_sixel("\x1b[?62;4c"));
        assert!(supports_sixel("\x1b[?64;1;2;4;6;9;15;22c"));
        assert!(!supports_sixel("\x1b[?62;40c"), "40 must not match 4");
        assert!(!supports_sixel("\x1b[?14c"));
    }
}
