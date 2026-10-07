//! The questions omaread asks a decision model.
//!
//! `decision` is the generic layer — state in, typed answers out, no idea what
//! a book is. This file is the opposite half: the one place that knows what a
//! `BookRecord` holds and how to describe a shelf to a model. Keeping the two
//! apart is the point. The layer can be lifted into another program untouched;
//! these two questions are this reader's, and would be replaced.
//!
//! Both questions are asked in one request each, not one per book, because the
//! protocol answers a map of questions about one piece of state. A shelf of a
//! few dozen books is one call.

use crate::decision::{Answer, Decision, Question, Transport};
use crate::identity::BookId;
use crate::library::Entry;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// How many books one ranking request carries. A bigger library is asked in
/// batches, so the request stays inside the model's context and a failure
/// costs one batch rather than the whole shelf.
const BATCH: usize = 60;

/// The scale a ranking answer comes back on: ten levels, "0" to "9". The
/// answer is the expected level, so a wider scale is finer and slower to ask;
/// ten is the protocol's limit (it refuses eleven), and ties break by title
/// anyway.
const LEVELS: [&str; 10] = [
    "0 (not at all)",
    "1",
    "2",
    "3",
    "4 (somewhat)",
    "5",
    "6",
    "7",
    "8",
    "9 (perfectly)",
];

/// A sort key per author, decided by the model.
///
/// Author names are the one field a naive sort gets wrong: "Murakami, Haruki"
/// and "Haruki Murakami" are one author written two ways, and only the reader
/// — or a model — knows which part of a name is the family name. The key puts
/// the family name first, so both spellings land together. An author the model
/// does not name falls back to the plain key, so a partial answer still sorts.
pub fn author_keys<T: Transport>(
    decision: &mut Decision<T>,
    authors: &[String],
) -> Result<HashMap<String, String>> {
    let authors: Vec<&str> = distinct(authors);
    if authors.is_empty() {
        return Ok(HashMap::new());
    }

    let mut questions = BTreeMap::new();
    for (index, _) in authors.iter().enumerate() {
        questions.insert(
            format!("a{index}"),
            Question::choice(
                format!(
                    "How is the name at index {index} in `authors` written? \
                     Answer so the family name can be sorted first."
                ),
                &[
                    (
                        "family_last",
                        "The family name is written last, as in 'Haruki Murakami'",
                    ),
                    (
                        "family_first",
                        "The family name is written first, as in 'Murakami, Haruki' or 'Murakami Haruki'",
                    ),
                    (
                        "single",
                        "A single name, a mononym, an organisation, or no clear split",
                    ),
                ],
            ),
        );
    }

    let answers = decision.ask(json!({ "authors": authors }), questions)?;
    let mut keys = HashMap::new();
    for (index, author) in authors.iter().enumerate() {
        let shape = answers
            .get(&format!("a{index}"))
            .and_then(Answer::choice)
            .unwrap_or("single");
        keys.insert((*author).to_string(), author_key(author, shape));
    }
    Ok(keys)
}

/// A score per book for a criterion the reader typed, highest first when sorted.
///
/// This is the super sort: "cozy for a rainy night", "shortest first", "the
/// ones I keep meaning to read". The criterion is free text; the model turns it
/// into a number per book, and the shelf sorts on the number.
pub fn rank<T: Transport>(
    decision: &mut Decision<T>,
    criterion: &str,
    entries: &[Entry],
) -> Result<HashMap<BookId, i64>> {
    if entries.is_empty() {
        return Ok(HashMap::new());
    }
    let mut scores = HashMap::new();
    for batch in entries.chunks(BATCH) {
        let books: Vec<Value> = batch.iter().map(brief).collect();
        let mut questions = BTreeMap::new();
        for index in 0..batch.len() {
            questions.insert(
                format!("b{index}"),
                Question::score(
                    format!(
                        "How well does the book at index {index} in `books` fit \
                         the criterion in `criterion`?"
                    ),
                    &LEVELS,
                ),
            );
        }
        let answers = decision.ask(json!({ "criterion": criterion, "books": books }), questions)?;
        for (index, entry) in batch.iter().enumerate() {
            let score = answers
                .get(&format!("b{index}"))
                .and_then(Answer::score)
                .map(|score| score.round() as i64);
            if let Some(score) = score {
                scores.insert(entry.id.clone(), score);
            }
        }
    }
    if scores.is_empty() {
        bail!("the decision model ranked none of the books");
    }
    Ok(scores)
}

/// What the model is shown about a book: enough to judge, and nothing that
/// would leak a path or an id into the request.
fn brief(entry: &Entry) -> Value {
    json!({
        "title": entry.record.display_title(),
        "authors": entry.record.display_authors(),
        "series": entry.record.series.clone().unwrap_or_default(),
        "tags": entry.record.tags.join(", "),
    })
}

/// The authors, once each and without the blanks, in the order they appear.
fn distinct(authors: &[String]) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    authors
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty() && seen.insert(*name))
        .collect()
}

/// The name as a key that sorts by family name.
fn author_key(author: &str, shape: &str) -> String {
    let lower = author.trim().to_lowercase();
    let plain = || lower.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    match shape {
        // "Haruki Murakami" -> "murakami haruki". A trailing particle or suffix
        // ("Jr.", "III") is not handled: at that point the model's answer is
        // doing as well as this rule can, and the tie breaks by title.
        "family_last" => match lower.rsplit_once(' ') {
            Some((given, family)) => format!("{family} {given}"),
            None => lower,
        },
        _ => plain(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_sorts_by_its_family_name() {
        assert_eq!(author_key("Haruki Murakami", "family_last"), "murakami haruki");
        assert_eq!(author_key("Murakami, Haruki", "family_first"), "murakami haruki");
        assert_eq!(author_key("村上春树", "single"), "村上春树");
        // Both spellings of one name become one key, which is the whole point.
        assert_eq!(
            author_key("Haruki Murakami", "family_last"),
            author_key("Murakami, Haruki", "family_first")
        );
    }

    #[test]
    fn authors_are_asked_once_each_and_blanks_are_skipped() {
        let names = vec![
            "Haruki Murakami".to_string(),
            "".to_string(),
            "  ".to_string(),
            "Haruki Murakami".to_string(),
            "Ursula K. Le Guin".to_string(),
        ];
        assert_eq!(distinct(&names), ["Haruki Murakami", "Ursula K. Le Guin"]);
    }
}

