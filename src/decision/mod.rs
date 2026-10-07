//! Asking a decision model.
//!
//! This is the whole of the reader's contact with an AI model, and it is
//! deliberately its own layer, small and with no idea what a book is. A caller
//! hands in a piece of `state` and a map of typed `questions`; the model answers
//! each question. What the caller does with the answers — order a shelf, pick a
//! voice, choose a route — is the caller's business.
//!
//! The shape is:
//!
//! ```text
//!   Decision::ask(state, questions)
//!        |  build the wire request (system_one)
//!        |  check the cache
//!        v
//!   Transport::post(url, body, headers)   <- curl today, anything tomorrow
//!        |
//!        v
//!   parse the answers (system_one)
//! ```
//!
//! The model is the TypeSafe "System One" classifier that OpenRouter proxies
//! (`typesafe/jev-1.13`). It is not a chat model: it answers only the typed
//! questions it is given, which is why a decision comes back structured rather
//! than as prose to be parsed. `system_one` holds those shapes; `transport`
//! holds the one place that touches the network.
//!
//! # Reusing this
//!
//! Nothing in this module names omaread. Copy `decision/` into another program,
//! give `Config` a base URL, a model id and a key, and hand `Decision` a
//! `Transport`. The cache and the graceful "no key, no model" behaviour come
//! along. What is specific to this reader — which questions to ask a shelf —
//! lives in `sorts`, beside it but outside the layer.

mod system_one;
mod transport;

pub use system_one::{Answer, Answers, Question};
pub use transport::{Curl, Transport};

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// The OpenRouter endpoint that serves the classifier, and the classifier.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_MODEL: &str = "typesafe/jev-1.13";

/// Where the model lives and how to reach it.
#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    /// `None` means no model is configured, and every sort falls back to the
    /// plain one. A missing key is not an error: the reader works offline.
    pub api_key: Option<String>,
    pub timeout: Duration,
}

impl Config {
    /// Resolves the settings from, in order, the explicit arguments (a config
    /// file), the environment, and the defaults. The key is searched for in
    /// `OMAREAD_DECISION_API_KEY`, then in the variable named by `api_key_env`,
    /// then in `OPENROUTER_API_KEY`, and last in the key pi already stores —
    /// see `pi_key`, which is an experiment's convenience and not a promise.
    pub fn resolve(
        base_url: Option<String>,
        model: Option<String>,
        api_key_env: Option<String>,
    ) -> Self {
        let base_url = env("OMAREAD_DECISION_BASE_URL")
            .or(base_url)
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let model = env("OMAREAD_DECISION_MODEL")
            .or(model)
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let api_key = env("OMAREAD_DECISION_API_KEY")
            .or_else(|| api_key_env.as_deref().and_then(env))
            .or_else(|| env("OPENROUTER_API_KEY"))
            .or_else(pi_key);
        Self {
            base_url,
            model,
            api_key,
            timeout: Duration::from_secs(45),
        }
    }

    /// Whether there is a key to ask with. Without one the reader still sorts,
    /// just without the model.
    pub fn available(&self) -> bool {
        self.api_key.is_some()
    }
}

/// The key pi stores for OpenRouter, read only when nothing else supplied one.
///
/// This is what makes the reader's model usable on a machine that already runs
/// pi, with no second place to keep a key. It is also the one line here that
/// reaches into another program's files, and a packaged reader should not: it
/// exists so an experiment needs no setup, and is meant to be deleted when that
/// stops being true.
fn pi_key() -> Option<String> {
    let path = dirs::home_dir()?.join(".pi/agent/auth.json");
    let text = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    json.pointer("/openrouter/key")?.as_str().map(str::to_string)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// A decision model: where it lives, how the request travels, and the answers
/// it has already given.
///
/// The cache is by the exact request, so asking the same sort twice — cycling
/// back to an order, reopening the shelf — costs one call for the session. It
/// is not written to disk: a model's answer is not a fact about the library, and
/// a stale one would be worse than a repeated call.
pub struct Decision<T: Transport = Curl> {
    config: Config,
    transport: T,
    cache: HashMap<[u8; 16], Answers>,
}

impl<T: Transport> Decision<T> {
    pub fn new(config: Config, transport: T) -> Self {
        Self {
            config,
            transport,
            cache: HashMap::new(),
        }
    }

    /// Whether a key is configured, so a caller can skip the question entirely.
    pub fn available(&self) -> bool {
        self.config.available()
    }

    /// Asks every question about `state` and returns the answers, by id.
    pub fn ask(&mut self, state: Value, questions: BTreeMap<String, Question>) -> Result<Answers> {
        let key = fingerprint(&self.config.model, &state, &questions);
        if let Some(answers) = self.cache.get(&key) {
            return Ok(answers.clone());
        }
        let api_key = self
            .config
            .api_key
            .as_deref()
            .context("no decision model is configured")?;
        let body = system_one::request(&self.config.model, &state, &questions).to_string();
        let url = format!("{}/systemone", self.config.base_url.trim_end_matches('/'));
        let authorization = format!("Bearer {api_key}");
        let headers = [
            ("Authorization", authorization.as_str()),
            ("content-type", "application/json"),
        ];
        let text = self
            .transport
            .post(&url, &body, &headers, self.config.timeout)?;
        let answers = system_one::parse_response(&text)?;
        self.cache.insert(key, answers.clone());
        Ok(answers)
    }
}

/// A stable short hash of a request, for the cache key.
fn fingerprint(model: &str, state: &Value, questions: &BTreeMap<String, Question>) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update(state.to_string().as_bytes());
    for (id, question) in questions {
        hasher.update(id.as_bytes());
        hasher.update(question.wire().to_string().as_bytes());
    }
    let digest = hasher.finalize();
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    /// One thing the fake was sent: the URL, the body, and the headers.
    type Sent = (String, String, Vec<(String, String)>);

    /// A transport that never leaves the process: it records what it was asked
    /// and answers from a script.
    struct Fake {
        reply: String,
        sent: RefCell<Vec<Sent>>,
    }

    impl Fake {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_string(),
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl Transport for Fake {
        fn post(
            &self,
            url: &str,
            body: &str,
            headers: &[(&str, &str)],
            _timeout: Duration,
        ) -> Result<String> {
            self.sent.borrow_mut().push((
                url.to_string(),
                body.to_string(),
                headers
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
            ));
            Ok(self.reply.clone())
        }
    }

    fn decision(reply: &str) -> Decision<Fake> {
        Decision::new(
            Config {
                base_url: "https://example.test/api/v1".into(),
                model: "test/model".into(),
                api_key: Some("secret".into()),
                timeout: Duration::from_secs(5),
            },
            Fake::new(reply),
        )
    }

    #[test]
    fn a_question_goes_out_and_an_answer_comes_back() {
        let mut model = decision(
            r#"{"answers":{"mood":{"type":"choice","choice":"cozy","probabilities":{"cozy":0.9,"bleak":0.1},"confidence":0.9},"rank":{"type":"score","score":2,"confidence":0.7},"worth":{"type":"noul","noul":0.8}}}"#,
        );
        let mut questions = BTreeMap::new();
        questions.insert(
            "mood".to_string(),
            Question::choice("How does it feel?", &[("cozy", "warm"), ("bleak", "cold")]),
        );
        questions.insert("rank".to_string(), Question::score("How good?", &["a", "b", "c"]));
        questions.insert("worth".to_string(), Question::bool("Worth it?", "yes", "no"));
        let answers = model
            .ask(json!({ "title": "A Book" }), questions)
            .expect("the fake always answers");

        assert_eq!(answers.get("mood").and_then(Answer::choice), Some("cozy"));
        assert_eq!(answers.get("rank").and_then(Answer::score), Some(2.0));
        assert_eq!(answers.get("worth").and_then(Answer::probability), Some(0.8));

        // The request carried the model, the key and the state, and the URL is
        // the base with `systemone` on the end.
        let sent = model.transport.sent.borrow();
        let (url, body, headers) = &sent[0];
        assert_eq!(url, "https://example.test/api/v1/systemone");
        assert!(body.contains(r#""model":"test/model""#), "{body}");
        assert!(body.contains(r#""title":"A Book""#), "{body}");
        // A bool is `noul` on the wire, whatever it is called here.
        assert!(body.contains(r#""type":"noul""#), "{body}");
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "Authorization" && value == "Bearer secret"),
            "{headers:?}"
        );
    }

    #[test]
    fn the_same_question_is_asked_once() {
        let mut model = decision(
            r#"{"answers":{"q":{"type":"score","score":1,"confidence":1}}}"#,
        );
        let ask = |model: &mut Decision<Fake>| {
            let mut questions = BTreeMap::new();
            questions.insert("q".to_string(), Question::score("How?", &["a", "b"]));
            model.ask(json!({ "x": 1 }), questions).unwrap()
        };
        ask(&mut model);
        ask(&mut model);
        assert_eq!(model.transport.sent.borrow().len(), 1, "the second is cached");
    }

    #[test]
    fn a_refusal_or_a_missing_key_is_an_error_not_a_panic() {
        let text = r#"{"error":{"message":"no such model"}}"#;
        assert!(system_one::parse_response(text).is_err());
        assert!(system_one::parse_response("not json").is_err());

        // No key: `ask` says so rather than reaching for the transport.
        let mut model = Decision::new(
            Config {
                api_key: None,
                ..Config::resolve(None, None, None)
            },
            Fake::new("{}"),
        );
        assert!(!model.available());
        assert!(model.ask(json!({}), BTreeMap::new()).is_err());
        assert!(model.transport.sent.borrow().is_empty());
    }
}
