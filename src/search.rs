//! Searching inside a book.
//!
//! The search runs over the parsed blocks, not the raw markup, so it finds what
//! the reader shows and never a tag name. Matching ignores case.
//!
//! Chapters are searched one at a time, starting from where the reader stands.
//! A book of a hundred chapters is not indexed up front: that would cost seconds
//! on opening, for something the reader may never ask for.

use crate::doc::Chapter;

/// Where a match sits: the block and the character offset inside it.
pub type Hit = (usize, usize);

#[derive(Debug, Clone, Default)]
pub struct Search {
    /// What was typed, as typed.
    query: String,
    /// Matches in the chapter currently laid out.
    hits: Vec<Hit>,
    /// Which of those the reader is on.
    current: Option<usize>,
}

impl Search {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn current(&self) -> Option<Hit> {
        self.current.and_then(|index| self.hits.get(index)).copied()
    }

    /// True when this position is the match the reader is on.
    pub fn is_current(&self, block: usize, offset: usize) -> bool {
        self.current() == Some((block, offset))
    }

    /// Number of characters in the query, which is how long a match is.
    pub fn query_len(&self) -> usize {
        self.query.chars().count()
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.hits.clear();
        self.current = None;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Takes over matches the reader already found, without reading the
    /// chapter again. `n` scans the chapter it is about to open — it has to
    /// know a match is in there before moving — and hands its finds in here.
    pub fn adopt(&mut self, hits: Vec<Hit>) {
        self.hits = hits;
        self.current = None;
    }

    /// Re-runs the search over a chapter, keeping the query.
    pub fn scan(&mut self, chapter: &Chapter) {
        self.adopt(find_all(chapter, &self.query));
    }

    /// Selects the first match at or after a position. Returns whether one was
    /// found in this chapter.
    pub fn go_to_first_after(&mut self, block: usize, offset: usize) -> bool {
        match self.hits.iter().position(|hit| *hit >= (block, offset)) {
            Some(index) => {
                self.current = Some(index);
                true
            }
            None => false,
        }
    }

    pub fn go_to_first(&mut self) -> bool {
        self.current = if self.hits.is_empty() { None } else { Some(0) };
        self.current.is_some()
    }

    pub fn go_to_last(&mut self) -> bool {
        self.current = self.hits.len().checked_sub(1);
        self.current.is_some()
    }

    /// Steps to the next match within this chapter. False when there is none, so
    /// the caller can move on to the next chapter.
    pub fn next_in_chapter(&mut self) -> bool {
        match self.current {
            Some(index) if index + 1 < self.hits.len() => {
                self.current = Some(index + 1);
                true
            }
            None if !self.hits.is_empty() => {
                self.current = Some(0);
                true
            }
            _ => false,
        }
    }

    pub fn previous_in_chapter(&mut self) -> bool {
        match self.current {
            Some(index) if index > 0 => {
                self.current = Some(index - 1);
                true
            }
            None if !self.hits.is_empty() => {
                self.current = Some(self.hits.len() - 1);
                true
            }
            _ => false,
        }
    }

    /// Which match of how many, for the status line.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.current.map(|index| (index + 1, self.hits.len()))
    }
}

/// Every match of `needle` in a chapter, in reading order.
///
/// Matching is case-insensitive. Offsets are character positions inside the
/// block, so the text and the query each fold one character into exactly one
/// lowercase character: a fold that widened — as `İ`'s does — would push every
/// offset after it off the character it names. The runs are walked as they
/// stand, so a block costs no copy of its own text to be searched.
pub fn find_all(chapter: &Chapter, needle: &str) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = needle.chars().map(folded).collect();
    let mut hits = Vec::new();
    // The one attempt in flight: the folded characters read so far, starting
    // at the block's character `at`. It never holds more than the query.
    let mut window: Vec<char> = Vec::with_capacity(needle.len());

    for (index, block) in chapter.blocks.iter().enumerate() {
        window.clear();
        let mut at = 0;
        for ch in block
            .runs
            .iter()
            .flat_map(|run| run.text.chars())
            .map(folded)
        {
            window.push(ch);
            if window.len() < needle.len() {
                continue;
            }
            // Overlapping matches are not reported twice: the search steps past a
            // match, as a reader would expect from `n`.
            if window[..] == needle[..] {
                hits.push((index, at));
                at += needle.len();
                window.clear();
            } else {
                at += 1;
                window.remove(0);
            }
        }
    }
    hits
}

/// One lowercase character for one character of the text, on both sides.
fn folded(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Block, BlockKind, Run, RunStyle};

    fn chapter(texts: &[&str]) -> Chapter {
        Chapter {
            links: Vec::new(),
            anchors: std::collections::HashMap::new(),
            href: "c.xhtml".into(),
            blocks: texts
                .iter()
                .map(|text| Block {
                    kind: BlockKind::Paragraph,
                    runs: vec![Run {
                        text: (*text).into(),
                        style: RunStyle::default(),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn finds_every_match_in_reading_order() {
        // Case is nobody's business, and one match inside another is counted
        // once: "aaaa" holds two non-overlapping "aa".
        let texts = chapter(&["the cat sat", "no match here", "cat again, cat"]);
        assert_eq!(find_all(&texts, "cat"), vec![(0, 4), (2, 0), (2, 11)]);
        let texts = chapter(&["The Phoenix Framework"]);
        assert_eq!(find_all(&texts, "phoenix"), vec![(0, 4)]);
        assert_eq!(find_all(&texts, "PHOENIX"), vec![(0, 4)]);
        let texts = chapter(&["aaaa"]);
        assert_eq!(find_all(&texts, "aa"), vec![(0, 0), (0, 2)]);
        // Nothing to look for finds nothing, and neither does a word the text
        // does not hold.
        let texts = chapter(&["text"]);
        assert!(find_all(&texts, "").is_empty());
        assert!(find_all(&texts, "abc").is_empty());
    }

    #[test]
    fn a_character_that_folds_wider_still_counts_as_one() {
        // 'İ' lowercases to two characters. Folding it to one keeps every
        // offset after it on the character the reader sees: the 'cat' of
        // "İcat" starts right after the İ.
        assert_eq!(find_all(&chapter(&["İcat"]), "cat"), vec![(0, 1)]);
        // The query folds by the same rule, so what is typed is what is found.
        assert_eq!(find_all(&chapter(&["icat"]), "İ"), vec![(0, 0)]);
    }

    #[test]
    fn the_cursor_steps_through_the_matches() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "cat"]));

        assert_eq!(search.hits().len(), 2);
        assert!(search.next_in_chapter());
        assert_eq!(search.current(), Some((0, 0)));
        assert!(search.next_in_chapter());
        assert_eq!(search.current(), Some((1, 0)));
        // Past the last one, so the caller moves to the next chapter.
        assert!(!search.next_in_chapter());
        assert_eq!(search.progress(), Some((2, 2)));

        // Backwards starts at the last one when nothing is current.
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "cat"]));
        assert!(search.previous_in_chapter());
        assert_eq!(search.current(), Some((1, 0)));
        assert!(search.previous_in_chapter());
        assert_eq!(search.current(), Some((0, 0)));
        assert!(!search.previous_in_chapter());
    }

    #[test]
    fn the_cursor_jumps_over_a_position() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "dog", "cat"]));
        assert!(search.go_to_first_after(1, 0));
        assert_eq!(search.current(), Some((2, 0)));
        // Nothing after the last match, so the caller looks further on.
        assert!(!search.go_to_first_after(3, 0));
    }

    #[test]
    fn a_new_query_drops_the_old_matches() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat"]));
        assert_eq!(search.hits().len(), 1);
        search.set_query("dog".into());
        assert!(search.hits().is_empty());
        assert!(search.current().is_none());
    }
}
