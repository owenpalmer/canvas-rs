//! Checkpoint mode for PDFs in the viewer, ported from CheckpointReader (github: pdf-checkpoints).
//!
//! Split a PDF anywhere and Claude writes retrieval-practice questions about the passage above the
//! split; the good ones go to Anki. This module holds the server side: the Anthropic API key, writing
//! the questions (streamed as they're written), and each PDF's checkpoints.
//!
//! Checkpoints are saved per PDF (by its fingerprint) as JSON, in the format CheckpointReader uses. A
//! PDF with none here yet shows CheckpointReader's, if you have that app; its files are only read.
//!
//! With CHECKPOINTS_MOCK=1 (or in the demo without a key), placeholder questions stand in for Claude.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{config, prefs};

pub const DEFAULT_MODEL: &str = "claude-opus-5";
pub fn models() -> Value {
    json!([
        {"id": "claude-opus-5", "name": "Claude Opus 5", "note": "default, $5/$25 per MTok"},
        {"id": "claude-opus-5-5", "name": "Claude Opus 5.5", "note": "$4/$20 per MTok"},
        {"id": "claude-fable-5-1", "name": "Claude Fable 5.1", "note": "most capable, $10/$50 per MTok"},
        {"id": "claude-sonnet-5", "name": "Claude Sonnet 5", "note": "faster, $2/$10 per MTok"},
        {"id": "claude-haiku-4-5", "name": "Claude Haiku 4.5", "note": "fastest, $1/$5 per MTok"},
    ])
}
/// Models without adaptive thinking or effort: they get a fixed thinking budget instead.
const BUDGET_THINKING_MODELS: &[&str] = &["claude-haiku-4-5"];
/// Models that take the server-side refusal fallback (the others just report a refusal).
const FALLBACK_MODELS: &[&str] = &["claude-opus-5", "claude-fable-5-1"];
const API: &str = "https://api.anthropic.com";

pub fn checkpoints_dir() -> PathBuf {
    config::data_dir().join("checkpoints")
}

fn known_model(m: &str) -> bool {
    models().as_array().unwrap().iter().any(|x| x["id"] == m)
}

pub fn current_model() -> String {
    match prefs::load().get("cpModel").and_then(|v| v.as_str()) {
        Some(m) if known_model(m) => m.to_string(),
        _ => DEFAULT_MODEL.to_string(),
    }
}

/// The model that writes answers; unset means the question model writes them too.
pub fn current_answer_model() -> String {
    match prefs::load().get("cpAnswerModel").and_then(|v| v.as_str()) {
        Some(m) if known_model(m) => m.to_string(),
        _ => current_model(),
    }
}

// --- the API key -----------------------------------------------------------------------------------
// ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN) in the environment wins; otherwise the key saved in
// Settings, kept in the system keychain, or in a file only you can read when there's no keychain.
const KEYRING_SERVICE: &str = "canvas-mcp";
const KEYRING_USER: &str = "anthropic_api_key";

fn secret_file() -> PathBuf {
    config::data_dir().join("anthropic_key.json")
}

#[derive(Clone, Default)]
struct Saved {
    key: Option<String>,
    source: Option<&'static str>,
    loaded: bool,
}
static SAVED: Lazy<Mutex<Saved>> = Lazy::new(|| Mutex::new(Saved::default()));

fn env_key() -> Option<(String, bool)> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Some((k, false));
        }
    }
    std::env::var("ANTHROPIC_AUTH_TOKEN").ok().filter(|k| !k.is_empty()).map(|k| (k, true))
}

fn keyring_entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()
}

/// Reads the keychain (which can block on D-Bus): call it off the UI thread the first time.
pub fn load_saved() {
    if SAVED.lock().unwrap().loaded {
        return;
    }
    let mut found = Saved { loaded: true, ..Default::default() };
    if let Some(key) = keyring_entry().and_then(|e| e.get_password().ok()).filter(|k| !k.is_empty()) {
        found.key = Some(key);
        found.source = Some("keychain");
    } else if let Some(key) = std::fs::read_to_string(secret_file()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["key"].as_str().map(String::from)) {
        found.key = Some(key);
        found.source = Some("file");
    }
    *SAVED.lock().unwrap() = found;
}

pub fn has_key() -> bool {
    load_saved();
    env_key().is_some() || SAVED.lock().unwrap().key.is_some()
}

pub fn is_mock() -> bool {
    std::env::var("CHECKPOINTS_MOCK").as_deref() == Ok("1") || (config::is_demo() && !has_key())
}

fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() > 14 { format!("{}…{}", chars[..7].iter().collect::<String>(), chars[chars.len() - 4..].iter().collect::<String>()) } else { "…".into() }
}

pub fn key_info() -> Value {
    load_saved();
    let saved = SAVED.lock().unwrap().clone();
    let (source, hint) = match env_key() {
        Some((k, _)) => (Some("env"), Some(mask(&k))),
        None => (saved.source, saved.key.as_deref().map(mask)),
    };
    json!({
        "source": source, "hint": hint, "installed": true, "mock": is_mock(),
        "allowed": config::is_demo() || config::allowed("claude"),
        "models": models(), "model": current_model(),
        "answer_model": prefs::load().get("cpAnswerModel").and_then(|v| v.as_str()).unwrap_or(""),
    })
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build().expect("http client")
}

fn auth(req: reqwest::RequestBuilder, key: &str, bearer: bool) -> reqwest::RequestBuilder {
    let req = req.header("anthropic-version", "2023-06-01");
    if bearer { req.bearer_auth(key) } else { req.header("x-api-key", key) }
}

/// Checks the key with Anthropic, then saves it. Returns an error message, or None.
pub async fn save_key(key: &str) -> Option<String> {
    let model = current_model();
    let resp = auth(http().get(format!("{API}/v1/models/{model}")), key, false).timeout(Duration::from_secs(20)).send().await;
    if let Ok(r) = &resp {
        match r.status().as_u16() {
            401 => return Some("Anthropic rejected that key.".into()),
            403 => return Some(format!("That key doesn't have access to {model}.")),
            _ => {} // offline or a hiccup: save it anyway
        }
    }
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        let stored = keyring_entry().map(|e| e.set_password(&key).is_ok()).unwrap_or(false)
            && keyring_entry().and_then(|e| e.get_password().ok()).as_deref() == Some(key.as_str());
        if stored {
            let _ = std::fs::remove_file(secret_file());
            *SAVED.lock().unwrap() = Saved { key: Some(key), source: Some("keychain"), loaded: true };
        } else {
            let _ = std::fs::create_dir_all(config::data_dir());
            let _ = std::fs::write(secret_file(), json!({"key": key}).to_string());
            config::private(&secret_file(), false);
            *SAVED.lock().unwrap() = Saved { key: Some(key), source: Some("file"), loaded: true };
        }
    })
    .await
    .ok();
    None
}

pub fn delete_key() {
    if let Some(e) = keyring_entry() {
        let _ = e.delete_credential();
    }
    let _ = std::fs::remove_file(secret_file());
    *SAVED.lock().unwrap() = Saved { key: None, source: None, loaded: true };
}

fn api_key() -> Option<(String, bool)> {
    load_saved();
    env_key().or_else(|| SAVED.lock().unwrap().key.clone().map(|k| (k, false)))
}

// --- each PDF's checkpoints ------------------------------------------------------------------------
fn file_name(key: &str) -> String {
    static BAD: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^A-Za-z0-9_-]").unwrap());
    let safe: String = BAD.replace_all(key, "_").chars().take(120).collect();
    format!("{safe}.json")
}

fn checkpointreader_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| config::home().join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        config::home().join("Library").join("Application Support")
    } else {
        std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| config::home().join(".local").join("share"))
    };
    base.join("checkpointreader").join("checkpoints")
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn load_state(key: &str) -> Value {
    let mine = checkpoints_dir().join(file_name(key));
    let v = if mine.exists() { read_json(&mine) } else { read_json(&checkpointreader_dir().join(file_name(key))) };
    match v {
        Some(Value::Array(a)) => Value::Array(a),
        _ => json!([]),
    }
}

pub fn save_state(key: &str, cuts: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(checkpoints_dir())?;
    let path = checkpoints_dir().join(file_name(key));
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, crate::util::pretty(cuts))?;
    std::fs::rename(tmp, path)
}

// --- writing questions -----------------------------------------------------------------------------
pub fn card_schema(with_answers: bool) -> Value {
    let (props, required) = if with_answers {
        (json!({"question": {"type": "string"}, "answer": {"type": "string"}}), json!(["question", "answer"]))
    } else {
        (json!({"question": {"type": "string"}}), json!(["question"]))
    };
    json!({
        "type": "object",
        "properties": {"cards": {"type": "array", "items": {"type": "object", "properties": props, "required": required, "additionalProperties": false}}},
        "required": ["cards"],
        "additionalProperties": false,
    })
}

const INPUT: &str = "You receive images of the passage the reader just finished (authoritative - use them for \
equations, figures and diagrams) plus its extracted text (may be garbled, especially math), \
and optionally some earlier material for background only.";

const FORMAT: &str = "- Write math as LaTeX inside \\( ... \\) (inline) or \\[ ... \\] (display). Do not use $ delimiters.
- For chemistry, write formulas and reactions with mhchem inside math, e.g. \\(\\ce{H2SO4}\\) or \\[\\ce{A + B -> C}\\].
- To show a molecular structure, write \\smiles{<SMILES>} (e.g. \\smiles{CC(=O)O}); it renders as a bond-line \
diagram. Use it when recognizing or reasoning about a structure is the point, in the question or the answer.";

fn system() -> String {
    format!(
        "You write retrieval-practice prompts that are embedded inline in a document, in the \
spirit of the \"mnemonic medium\": the reader has just finished a passage and answers a few \
questions to check and reinforce understanding. The good ones become spaced-repetition flashcards.

{INPUT}

Write 2-4 prompts about THE PASSAGE ONLY:
- Each prompt targets one idea and has a short, unambiguous answer (a phrase, formula or 1-2 sentences).
- Favor understanding over trivia: why/how, relationships, what changes if..., interpreting a formula or figure.
- Include at most one pure-recall definition prompt.
- The question must make sense on its own months later, without the document open.
{FORMAT}
- If the passage is only headers/boilerplate with nothing to learn, return an empty list."
    )
}

const QUESTIONS_ONLY: &str = "\n\nWrite only the questions; the answers are written separately.";

fn answer_system() -> String {
    format!(
        "You write the answer side of a retrieval-practice flashcard. The reader has just \
finished a passage of a document and will check their recall against your answer.

{INPUT}

Answer the given prompt from the passage:
- Keep it short and unambiguous: a phrase, formula or 1-2 sentences, plus a brief reason only if the \
prompt asks why or how.
- Reply with the answer alone: no restating the question, no preamble.
{FORMAT}"
    )
}

fn with_custom(system: &str) -> String {
    let custom: String = prefs::load().get("cpPrompt").and_then(|v| v.as_str()).unwrap_or("").trim().chars().take(4000).collect();
    if custom.is_empty() {
        return system.to_string();
    }
    format!("{system}\n\nAdditional instructions from the reader (follow them unless they conflict with the output format above):\n{custom}")
}

/// Picks each {"question", "answer"} object out of the streamed JSON ({"cards": [{...}, ...]})
/// as soon as its closing brace arrives: the cards are the objects at brace depth 2.
#[derive(Default)]
pub struct CardParser {
    buf: Vec<char>,
    depth: i32,
    start: usize,
    in_str: bool,
    escaped: bool,
}

impl CardParser {
    pub fn feed(&mut self, chunk: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let base = self.buf.len();
        self.buf.extend(chunk.chars());
        for i in base..self.buf.len() {
            let ch = self.buf[i];
            if self.in_str {
                if self.escaped {
                    self.escaped = false;
                } else if ch == '\\' {
                    self.escaped = true;
                } else if ch == '"' {
                    self.in_str = false;
                }
            } else if ch == '"' {
                self.in_str = true;
            } else if ch == '{' {
                self.depth += 1;
                if self.depth == 2 {
                    self.start = i;
                }
            } else if ch == '}' {
                if self.depth == 2 {
                    let s: String = self.buf[self.start..=i].iter().collect();
                    if let Ok(v) = serde_json::from_str(&s) {
                        out.push(v);
                    }
                }
                self.depth -= 1;
            }
        }
        out
    }
}

/// The passage as content blocks, shared by the question request and every answer request.
pub fn passage_blocks(body: &Value) -> Vec<Value> {
    let mut blocks: Vec<Value> = body["images"]
        .as_array()
        .into_iter()
        .flatten()
        .take(4)
        .map(|b64| json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": b64}}))
        .collect();
    let context = crate::util::s(&body["context_text"]).trim().to_string();
    let bg = if context.is_empty() { String::new() } else { format!("<background>\n{context}\n</background>\n\n") };
    blocks.push(json!({
        "type": "text",
        "text": format!(
            "Document: {}\nPassage location: {}\n\n{bg}<passage_text>\n{}\n</passage_text>\n\nThe images above show the passage.",
            body["doc_title"].as_str().unwrap_or("untitled"),
            body["location"].as_str().unwrap_or(""),
            crate::util::s(&body["section_text"]).trim()
        ),
    }));
    blocks
}

/// Per-model request settings: thinking style, effort, output format and refusal fallback.
/// Returns (body fields, beta headers).
pub fn model_args(model: &str, schema: Option<Value>) -> (serde_json::Map<String, Value>, Vec<&'static str>) {
    let mut args = serde_json::Map::new();
    args.insert("model".into(), json!(model));
    args.insert("max_tokens".into(), json!(16000));
    if BUDGET_THINKING_MODELS.contains(&model) {
        args.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": 4000}));
        if let Some(s) = schema {
            args.insert("output_config".into(), json!({"format": {"type": "json_schema", "schema": s}}));
        }
    } else {
        args.insert("thinking".into(), json!({"type": "adaptive"}));
        let mut oc = json!({"effort": "medium"});
        if let Some(s) = schema {
            oc["format"] = json!({"type": "json_schema", "schema": s});
        }
        args.insert("output_config".into(), oc);
    }
    let mut betas = Vec::new();
    if FALLBACK_MODELS.contains(&model) {
        betas.push("server-side-fallback-2026-07-01");
        args.insert("fallbacks".into(), json!("default"));
    }
    (args, betas)
}

/// A failed request, as (status for the UI, message).
#[derive(Clone, Debug)]
pub struct GenError {
    pub kind: &'static str,
    pub status: u16,
    pub message: String,
}

impl GenError {
    fn new(kind: &'static str, status: u16, message: impl Into<String>) -> GenError {
        GenError { kind, status, message: message.into() }
    }
}

fn api_error(status: u16, body: &str) -> GenError {
    let msg = serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| body.chars().take(300).collect());
    match status {
        401 => GenError::new("generate", 401, "Anthropic didn't accept the API key. Check it in Settings."),
        429 => GenError::new("generate", 429, "Anthropic is rate limiting the key. Try again shortly."),
        _ => GenError::new("generate", 502, format!("Anthropic API error {status}: {msg}")),
    }
}

fn connection_error() -> GenError {
    GenError::new("generate", 502, "Couldn't reach the Anthropic API.")
}

async fn post_messages(req_body: Value, betas: &[&str], stream: bool) -> Result<reqwest::Response, GenError> {
    let (key, bearer) = api_key().ok_or_else(|| GenError::new("key", 401, "Add your Anthropic API key in Settings."))?;
    let mut body = req_body;
    if stream {
        body["stream"] = json!(true);
    }
    let mut req = auth(http().post(format!("{API}/v1/messages?beta=true")), &key, bearer).json(&body);
    if !betas.is_empty() {
        req = req.header("anthropic-beta", betas.join(","));
    }
    let resp = req.send().await.map_err(|_| connection_error())?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(api_error(status, &text));
    }
    Ok(resp)
}

async fn write_answer(passage: Vec<Value>, question: String, model: String) -> String {
    // The passage prefix is cached, so answering several questions about it only pays for it once.
    let mut cached = passage;
    if let Some(last) = cached.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    cached.push(json!({"type": "text", "text": format!("Prompt: {question}")}));
    let (mut args, betas) = model_args(&model, None);
    args.insert("system".into(), json!(with_custom(&answer_system())));
    args.insert("messages".into(), json!([{"role": "user", "content": cached}]));
    let resp = match post_messages(Value::Object(args), &betas, false).await {
        Ok(r) => r,
        Err(e) => return format!("(Couldn't write an answer: {})", e.message),
    };
    let Ok(v) = resp.json::<Value>().await else { return "(Couldn't write an answer: Couldn't reach the Anthropic API.)".into() };
    if v["stop_reason"] == "refusal" {
        return "(Claude declined to answer this one.)".into();
    }
    v["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "text").map(|b| b["text"].as_str().unwrap_or("")).collect::<String>().trim().to_string()
}

/// Whether questions can be written right now: (kind, status, message) if not.
pub fn can_generate() -> Result<(), GenError> {
    if is_mock() {
        return Ok(());
    }
    if !config::is_demo() && !config::allowed("claude") {
        return Err(GenError::new("permission", 403, "Allow the app to send passages to Claude first."));
    }
    if !has_key() {
        return Err(GenError::new("key", 401, "Add your Anthropic API key in Settings."));
    }
    Ok(())
}

/// Write questions: events arrive on the channel as {"i", "card"} as each question is written,
/// {"i", "answer"} when a separate answer model writes the answers, then {"done": true, ...}.
/// An error ends the stream.
pub fn generate(body: Value) -> mpsc::UnboundedReceiver<Result<Value, GenError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        // the key may come from the system keychain, which blocks
        let check = tokio::task::spawn_blocking(|| can_generate().map(|_| is_mock())).await;
        let mock_ = match check {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                let _ = tx.send(Err(e));
                return;
            }
            Err(_) => {
                let _ = tx.send(Err(GenError::new("key", 401, "Couldn't read the API key.")));
                return;
            }
        };
        let result = if mock_ { mock(&body, &tx).await } else { real(&body, &tx).await };
        if let Err(e) = result {
            let _ = tx.send(Err(e));
        }
    });
    rx
}

async fn real(body: &Value, tx: &mpsc::UnboundedSender<Result<Value, GenError>>) -> Result<(), GenError> {
    let passage = passage_blocks(body);
    let count = body["count"].as_i64();
    let avoid: Vec<String> = body["avoid"].as_array().into_iter().flatten().map(crate::util::s).filter(|q| !q.trim().is_empty()).collect();
    let mut ask = match count {
        Some(n) if n > 0 => format!("Write exactly {n} prompt(s)."),
        _ => "Write the prompts.".to_string(),
    };
    if !avoid.is_empty() {
        ask += " They must test something different from these existing prompts:\n";
        ask += &avoid.iter().map(|q| format!("- {q}")).collect::<Vec<_>>().join("\n");
    }
    let (model, answer_model) = (current_model(), current_answer_model());
    let separate = answer_model != model;
    let (mut args, betas) = model_args(&model, Some(card_schema(!separate)));
    args.insert("system".into(), json!(with_custom(&(system() + if separate { QUESTIONS_ONLY } else { "" }))));
    let mut content = passage.clone();
    content.push(json!({"type": "text", "text": ask}));
    args.insert("messages".into(), json!([{"role": "user", "content": content}]));
    let resp = post_messages(Value::Object(args), &betas, true).await?;

    let mut answers: Vec<(usize, tokio::task::JoinHandle<String>)> = Vec::new();
    let mut parser = CardParser::default();
    let mut n = 0usize;
    let mut stop_reason = String::new();
    let mut resp_model = model.clone();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut data = String::new();
    let flush = |answers: &mut Vec<(usize, tokio::task::JoinHandle<String>)>| {
        let mut out = Vec::new();
        answers.retain_mut(|(i, h)| {
            if h.is_finished() {
                out.push((*i, futures::FutureExt::now_or_never(h).and_then(|r| r.ok()).unwrap_or_default()));
                false
            } else {
                true
            }
        });
        out
    };
    'read: while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| connection_error())?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end_matches('\r').to_string();
            buf.drain(..=nl);
            if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim_start());
                continue;
            }
            if !line.is_empty() || data.is_empty() {
                continue;
            }
            let ev: Value = serde_json::from_str(&std::mem::take(&mut data)).unwrap_or(Value::Null);
            match ev["type"].as_str().unwrap_or("") {
                "message_start" => {
                    if let Some(m) = ev["message"]["model"].as_str() {
                        resp_model = m.to_string();
                    }
                }
                "content_block_delta" if ev["delta"]["type"] == "text_delta" => {
                    for mut card in parser.feed(ev["delta"]["text"].as_str().unwrap_or("")) {
                        if separate {
                            let q = crate::util::s(&card["question"]);
                            answers.push((n, tokio::spawn(write_answer(passage.clone(), q, answer_model.clone()))));
                            card["answer"] = Value::Null;
                        }
                        let _ = tx.send(Ok(json!({"i": n, "card": card})));
                        n += 1;
                    }
                    for (i, a) in flush(&mut answers) {
                        let _ = tx.send(Ok(json!({"i": i, "answer": a})));
                    }
                }
                "message_delta" => {
                    if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                        stop_reason = r.to_string();
                    }
                }
                "error" => {
                    let msg = ev["error"]["message"].as_str().unwrap_or("stream error").to_string();
                    return Err(GenError::new("generate", 502, format!("Anthropic API error: {msg}")));
                }
                "message_stop" => break 'read,
                _ => {}
            }
        }
    }
    if stop_reason == "refusal" {
        for (_, h) in &answers {
            h.abort();
        }
        return Err(GenError::new("generate", 422, "Claude declined to write questions for this passage."));
    }
    if stop_reason == "max_tokens" {
        for (_, h) in &answers {
            h.abort();
        }
        return Err(GenError::new("generate", 422, "The response was cut off (max_tokens)."));
    }
    for (i, h) in answers {
        let a = h.await.unwrap_or_default();
        let _ = tx.send(Ok(json!({"i": i, "answer": a})));
    }
    let _ = tx.send(Ok(json!({"done": true, "mock": false, "model": resp_model})));
    Ok(())
}

/// Imitates the separate-answer-model flow: questions first, answers a little later.
async fn mock(body: &Value, tx: &mpsc::UnboundedSender<Result<Value, GenError>>) -> Result<(), GenError> {
    let sleep = |ms| tokio::time::sleep(Duration::from_millis(ms));
    if body["count"].as_i64().unwrap_or(0) > 0 {
        let n = 100 + rand::random::<u32>() % 900;
        let _ = tx.send(Ok(json!({"i": 0, "card": {"question": format!("(mock) Regenerated question #{n}: what is the key idea of this passage?"), "answer": null}})));
        sleep(800).await;
        let _ = tx.send(Ok(json!({"i": 0, "answer": format!("(mock answer #{n})")})));
        let _ = tx.send(Ok(json!({"done": true, "mock": true, "model": "mock"})));
        return Ok(());
    }
    static WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());
    static SENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"[.!?]\s+").unwrap());
    let text = WS.replace_all(&crate::util::s(&body["section_text"]), " ").trim().to_string();
    // split after sentence punctuation (the Python (?<=[.!?])\s+)
    let mut sentences = Vec::new();
    let mut last = 0;
    for m in SENT.find_iter(&text) {
        sentences.push(text[last..m.start() + 1].trim().to_string());
        last = m.end();
    }
    sentences.push(text[last..].trim().to_string());
    let mut cards: Vec<(String, String)> = sentences
        .into_iter()
        .filter(|s| s.chars().count() > 40)
        .take(2)
        .map(|s| {
            let head: String = s.chars().take(120).collect();
            let more = if s.chars().count() > 120 { "…" } else { "" };
            (format!("(mock) Explain in your own words: “{head}{more}”"), "(mock answer: add an API key in Settings for real questions)".to_string())
        })
        .collect();
    cards.push(("(mock) Math rendering check: what is the impedance of a capacitor?".into(), r"\(Z_C = \dfrac{1}{j\omega C}\)".into()));
    cards.push((
        r"(mock) Chemistry rendering check: name this molecule. \smiles{CC(=O)OC1=CC=CC=C1C(=O)O}".into(),
        r"Aspirin (acetylsalicylic acid), \(\ce{C9H8O4}\). It's made by acetylating salicylic acid: \[\ce{C7H6O3 + (CH3CO)2O -> C9H8O4 + CH3COOH}\] \smiles{OC(=O)C1=CC=CC=C1O}".into(),
    ));
    for (i, (q, _)) in cards.iter().enumerate() {
        sleep(500).await;
        let _ = tx.send(Ok(json!({"i": i, "card": {"question": q, "answer": null}})));
    }
    for (i, (_, a)) in cards.iter().enumerate() {
        sleep(600).await;
        let _ = tx.send(Ok(json!({"i": i, "answer": a})));
    }
    let _ = tx.send(Ok(json!({"done": true, "mock": true, "model": "mock"})));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_streams_to_done() {
        unsafe { std::env::set_var("CHECKPOINTS_MOCK", "1") };
        let mut rx = generate(json!({"section_text": "The heap property says every parent is no larger than its children. Insertion bubbles the new key up."}));
        let mut n = 0;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            let v = ev.map_err(|e| e.message).unwrap();
            n += 1;
            done |= v["done"] == true;
        }
        assert!(done && n > 4, "{n} events, done {done}");
    }

    #[test]
    fn card_parser_streams() {
        let mut p = CardParser::default();
        assert!(p.feed("{\"cards\": [{\"question\": \"a {b}\", \"ans").is_empty());
        let got = p.feed("wer\": \"x\\\"y\"}, {\"question\": \"c\"");
        assert_eq!(got, vec![json!({"question": "a {b}", "answer": "x\"y"})]);
        let got = p.feed(", \"answer\": \"d\"}]}");
        assert_eq!(got, vec![json!({"question": "c", "answer": "d"})]);
    }

    #[test]
    fn names() {
        assert_eq!(file_name("pdfcp:abc/def"), "pdfcp_abc_def.json");
        assert_eq!(mask("sk-ant-api03-abcdefghijk"), "sk-ant-…hijk");
        assert_eq!(mask("short"), "…");
    }

    #[test]
    fn args_per_model() {
        let (a, b) = model_args("claude-haiku-4-5", None);
        assert_eq!(a["thinking"]["type"], "enabled");
        assert!(b.is_empty());
        let (a, b) = model_args("claude-opus-5", Some(card_schema(true)));
        assert_eq!(a["output_config"]["effort"], "medium");
        assert_eq!(a["fallbacks"], "default");
        assert_eq!(b, vec!["server-side-fallback-2026-07-01"]);
    }
}
