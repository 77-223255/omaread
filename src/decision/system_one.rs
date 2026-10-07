//! The System One classifier protocol.
//!
//! A decision model is asked typed questions about a piece of structured data
//! (`state`), and answers each question in a map keyed by question id. This is
//! the protocol TypeSafe serves and OpenRouter proxies: one `POST` of JSON to
//! `<base-url>/systemone`, carrying `{ model, state, questions }`.
//!
//! This file knows the wire shapes and nothing about HTTP — where the request
//! travels is `transport` — so the same shapes can be served by a fake in a
//! test and by curl in the reader. The three question types are the ones the
//! protocol offers, and they are enough for every decision omaread asks:
//! `choice` to name a thing, `score` to rank it, `bool` to judge it.
//!
//! The layer speaks the whole protocol rather than only the parts this reader
//! happens to use: a program that copies `decision/` should start with the
//! full vocabulary, not with the subset one caller needed.
#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// A question for the decision model.
#[derive(Debug, Clone)]
pub enum Question {
    /// Pick one label. Each label is described, so the model knows what it
    /// means; the answer names the label it picked.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Score on an ordered scale, lowest first. The answer is the expected
    /// level index, from zero to `criteria.len() - 1`.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Yes or no. The answer is the probability of yes.
    Bool { instructions: String, yes: String, no: String },
}

impl Question {
    pub fn choice(instructions: impl Into<String>, criteria: &[(&str, &str)]) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            criteria: criteria
                .iter()
                .map(|(label, meaning)| ((*label).to_string(), (*meaning).to_string()))
                .collect(),
        }
    }

    pub fn score(instructions: impl Into<String>, criteria: &[&str]) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: criteria.iter().map(|level| (*level).to_string()).collect(),
        }
    }

    pub fn bool(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Bool {
            instructions: instructions.into(),
            yes: yes.into(),
            no: no.into(),
        }
    }

    pub(super) fn wire(&self) -> Value {
        match self {
            Question::Choice {
                instructions,
                criteria,
            } => json!({
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }),
            Question::Score {
                instructions,
                criteria,
            } => json!({
                "type": "score",
                "instructions": instructions,
                "criteria": criteria,
            }),
            // System One spells a boolean `noul` on the wire, and describes
            // both sides rather than only the true one.
            Question::Bool {
                instructions,
                yes,
                no,
            } => json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": { "true": yes, "false": no },
            }),
        }
    }
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
    Bool {
        probability: f64,
    },
}

impl Answer {
    /// The label a choice question picked.
    pub fn choice(&self) -> Option<&str> {
        match self {
            Answer::Choice { choice, .. } => Some(choice),
            _ => None,
        }
    }

    /// The level a score question settled on.
    pub fn score(&self) -> Option<f64> {
        match self {
            Answer::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// How likely the model thought the answer yes.
    pub fn probability(&self) -> Option<f64> {
        match self {
            Answer::Bool { probability } => Some(*probability),
            _ => None,
        }
    }

    /// How sure the model was, where it says so.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } => {
                Some(*confidence)
            }
            Answer::Bool { .. } => None,
        }
    }
}

/// The answers to one request, by question id.
#[derive(Debug, Clone, Default)]
pub struct Answers(BTreeMap<String, Answer>);

impl Answers {
    pub fn get(&self, id: &str) -> Option<&Answer> {
        self.0.get(id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Builds the request body for one round of questions.
pub fn request(model: &str, state: &Value, questions: &BTreeMap<String, Question>) -> Value {
    let questions: Map<String, Value> = questions
        .iter()
        .map(|(id, question)| (id.clone(), question.wire()))
        .collect();
    json!({ "model": model, "state": state, "questions": questions })
}

/// Reads the answers out of the response body, or the error that came instead.
pub fn parse_response(text: &str) -> Result<Answers> {
    let body: Value =
        serde_json::from_str(text).context("the decision model answered with something not JSON")?;
    if let Some(message) = body.pointer("/error/message").and_then(Value::as_str) {
        bail!("the decision model refused: {message}");
    }
    let answers = body
        .get("answers")
        .and_then(Value::as_object)
        .context("the decision model answered with no answers")?;
    let mut out = BTreeMap::new();
    for (id, value) in answers {
        out.insert(id.clone(), parse_answer(id, value)?);
    }
    Ok(Answers(out))
}

fn parse_answer(id: &str, value: &Value) -> Result<Answer> {
    let shape = value.get("type").and_then(Value::as_str).unwrap_or("");
    match shape {
        "choice" => {
            let choice = value
                .get("choice")
                .and_then(Value::as_str)
                .with_context(|| format!("answer {id:?} has no choice"))?
                .to_string();
            let probabilities = value
                .get("probabilities")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(label, p)| p.as_f64().map(|p| (label.clone(), p)))
                        .collect()
                })
                .unwrap_or_default();
            let confidence = value
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            Ok(Answer::Choice {
                choice,
                probabilities,
                confidence,
            })
        }
        "score" => {
            let score = value
                .get("score")
                .and_then(Value::as_f64)
                .with_context(|| format!("answer {id:?} has no score"))?;
            let confidence = value
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            Ok(Answer::Score { score, confidence })
        }
        // A boolean comes back as `noul`, holding the probability of true.
        "noul" => {
            let probability = value
                .get("noul")
                .and_then(Value::as_f64)
                .with_context(|| format!("answer {id:?} has no noul"))?;
            Ok(Answer::Bool { probability })
        }
        other => bail!("answer {id:?} has an unknown type {other:?}"),
    }
}
