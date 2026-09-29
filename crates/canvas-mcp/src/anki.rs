//! Review Anki cards in the app, through the AnkiConnect add-on of a running Anki.
//!
//! Anki stays the source of truth: cards are scheduled by Anki's own scheduler (answerCards), and
//! the deck list's new/learning/review counts are Anki's, daily limits included. The app keeps only
//! which deck belongs to which course (a deck it created, or one of yours you linked), by deck id, so
//! a deck renamed or moved in Anki stays linked.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::engine::Engine;
use crate::store::now;
use crate::util::s;
use crate::{Error, Result, anki_setup, config};

pub const EASES: [i64; 4] = [1, 2, 3, 4]; // Again, Hard, Good, Easy
const LEARN_AHEAD: f64 = 20.0 * 60.0; // Anki's default "learn ahead limit"

/// The note type for checkpoint cards: CheckpointReader's, so cards from that app and this one match.
/// The hint (the passage the question is about) stays hidden until you click "Show Hint".
pub fn checkpoint_note() -> Value {
    json!({
        "modelName": "PDF Checkpoint",
        "inOrderFields": ["Front", "Back", "Hint", "Source"],
        "css": ".card{font-family:system-ui,sans-serif;font-size:20px;text-align:center;color:#222;background:#fff}\n.hint img,.hint-img img{max-width:100%;border:1px solid #ddd;border-radius:6px;margin-top:10px}\n.src{font-size:13px;color:#888;margin-top:14px}\nimg.mol{display:block;margin:8px auto;max-width:240px;border-radius:6px}",
        "cardTemplates": [{
            "Name": "Card 1",
            "Front": "{{Front}}<div class=\"hint\">{{hint:Hint}}</div>",
            "Back": "{{Front}}<hr id=\"answer\">{{Back}}<div class=\"hint-img\">{{hint:Hint}}</div><div class=\"src\">{{Source}}</div>",
        }],
    })
}

/// A deck name as a search term (quotes, backslashes and the wildcards * and _ escaped).
pub fn q(name: &str) -> String {
    let mut n = name.to_string();
    for ch in ['\\', '"', '*', '_'] {
        n = n.replace(ch, &format!("\\{ch}"));
    }
    format!("\"deck:{n}\"")
}

pub struct Anki {
    pub engine: Arc<Engine>,
    http: reqwest::Client,
}

pub fn guess_type(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/x-wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "json" => "application/json",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

impl Anki {
    pub fn new(engine: Arc<Engine>) -> Arc<Anki> {
        engine.store.exec("CREATE TABLE IF NOT EXISTS anki_decks (course_id INTEGER PRIMARY KEY, deck_id INTEGER, deck_name TEXT)", &[]);
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client");
        Arc::new(Anki { engine, http })
    }

    pub async fn call(&self, action: &str, params: Value) -> Result<Value> {
        self.call_timeout(action, params, Duration::from_secs(10)).await
    }

    pub async fn call_timeout(&self, action: &str, params: Value, timeout: Duration) -> Result<Value> {
        let mut body = json!({"action": action, "version": 6, "params": if params.is_null() { json!({}) } else { params }});
        let settings = config::settings();
        if let Some(key) = settings.anki_api_key.clone().or_else(anki_setup::api_key) {
            body["key"] = json!(key);
        }
        let url = settings.anki_url.clone().unwrap_or_else(anki_setup::url);
        let resp = match self.http.post(&url).json(&body).timeout(timeout).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() || e.is_request() => {
                return Err(Error::AnkiOffline("Anki isn't running (or the AnkiConnect add-on isn't installed)".into()));
            }
            Err(e) => return Err(Error::AnkiOffline(format!("Anki isn't running (or the AnkiConnect add-on isn't installed): {e}"))),
        };
        let text = resp.text().await.map_err(|_| Error::AnkiOffline("Anki isn't running (or the AnkiConnect add-on isn't installed)".into()))?;
        let data: Value = serde_json::from_str(&text).map_err(|_| Error::AnkiPortTaken(url.clone()))?;
        let Some(obj) = data.as_object().filter(|o| o.contains_key("result")) else {
            return Err(Error::AnkiPortTaken(url)); // not AnkiConnect's {"result", "error"} reply
        };
        if let Some(err) = obj.get("error").filter(|e| crate::util::truthy(e)) {
            return Err(Error::Anki(s(err)));
        }
        Ok(obj["result"].clone())
    }

    // --- one deck per enrolled course --------------------------------------------------------------
    /// course id -> deck id
    pub fn course_decks(&self) -> HashMap<i64, i64> {
        self.engine.store.with(|db| {
            let mut stmt = db.prepare("SELECT course_id, deck_id FROM anki_decks").unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
        })
    }

    pub fn deck_name_for(course: &Value) -> String {
        let code = course["course_code"].as_str().filter(|c| !c.is_empty()).or(course["name"].as_str()).unwrap_or("");
        let name = code.replace("::", ":").trim().to_string();
        let parent = config::settings().anki_parent_deck;
        if parent.is_empty() { name } else { format!("{parent}::{name}") }
    }

    /// Point courses at decks: {course id: existing deck id, "new" to create one, or null to
    /// unlink}. Returns the names of decks created.
    pub async fn set_course_decks(&self, links: Vec<(i64, Value)>) -> Result<Vec<String>> {
        let e = &self.engine;
        let courses: Vec<Value> = e.cached_list("courses", &[]).into_iter().chain(e.cached_list("past_courses", &[])).collect();
        let by_id: HashMap<i64, Value> = courses.into_iter().filter_map(|c| c["id"].as_i64().map(|i| (i, c))).collect();
        let ids: Vec<i64> = self.call("deckNamesAndIds", json!({})).await?.as_object().map(|o| o.values().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
        let mut created = Vec::new();
        for (cid, target) in links {
            if target.is_null() {
                e.store.exec("DELETE FROM anki_decks WHERE course_id=?", &[&cid]);
                continue;
            }
            let (did, name): (i64, Option<String>) = if target == "new" {
                let Some(course) = by_id.get(&cid) else { continue };
                let name = Self::deck_name_for(course);
                // returns the existing deck's id if there is one
                let did = self.call("createDeck", json!({"deck": name})).await?.as_i64().unwrap_or(0);
                if !ids.contains(&did) {
                    created.push(name.clone());
                }
                (did, Some(name))
            } else {
                let t = target.as_i64().or_else(|| target.as_str().and_then(|x| x.parse().ok())).unwrap_or(-1);
                if !ids.contains(&t) {
                    return Err(Error::NotFound("That deck no longer exists in Anki".into()));
                }
                (t, None)
            };
            e.store.exec("INSERT OR REPLACE INTO anki_decks (course_id, deck_id, deck_name) VALUES (?,?,?)", &[&cid, &did, &name]);
        }
        Ok(created)
    }

    // --- decks ---------------------------------------------------------------------------------------
    pub async fn decks(&self) -> Result<Vec<Value>> {
        let by_name = self.call("deckNamesAndIds", json!({})).await?;
        let by_name: BTreeMap<String, i64> = by_name.as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_i64().map(|i| (k.clone(), i))).collect()).unwrap_or_default();
        let names: Vec<&String> = by_name.keys().collect();
        let stats = self.call("getDeckStats", json!({"decks": names})).await?;
        let course_of: HashMap<i64, i64> = self.course_decks().into_iter().map(|(c, d)| (d, c)).collect();
        let mut sorted: Vec<(&String, &i64)> = by_name.iter().collect();
        sorted.sort_by_key(|(n, _)| n.to_lowercase());
        let mut out: Vec<Value> = sorted
            .into_iter()
            .map(|(name, did)| {
                let st = stats.get(did.to_string()).cloned().unwrap_or(json!({}));
                json!({
                    "id": did, "name": name, "course_id": course_of.get(did),
                    "new": st["new_count"].as_i64().unwrap_or(0), "learn": st["learn_count"].as_i64().unwrap_or(0), "review": st["review_count"].as_i64().unwrap_or(0),
                    "total": st.get("total_in_deck").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        // Anki's total counts only the deck's own cards; include its subdecks'.
        let own: Vec<(String, i64)> = out.iter().map(|d| (s(&d["name"]), d["total"].as_i64().unwrap_or(0))).collect();
        for d in &mut out {
            if !d["total"].is_null() {
                let name = s(&d["name"]);
                let prefix = format!("{name}::");
                d["total"] = json!(own.iter().filter(|(n, _)| *n == name || n.starts_with(&prefix)).map(|(_, t)| t).sum::<i64>());
            }
        }
        Ok(out)
    }

    pub async fn deck_name(&self, deck_id: i64) -> Result<String> {
        let decks = self.call("deckNamesAndIds", json!({})).await?;
        for (name, did) in decks.as_object().into_iter().flatten() {
            if did.as_i64() == Some(deck_id) {
                return Ok(name.clone());
            }
        }
        Err(Error::NotFound("That deck no longer exists in Anki".into()))
    }

    // --- reviewing -------------------------------------------------------------------------------------
    /// The next card Anki would show for this deck: due learning cards, then reviews, then new
    /// cards (within the deck's daily limits). Includes the deck's remaining counts.
    pub async fn next_card(&self, deck_id: i64) -> Result<Value> {
        let name = self.deck_name(deck_id).await?;
        let stats = self.call("getDeckStats", json!({"decks": [name]})).await?.get(deck_id.to_string()).cloned().unwrap_or(json!({}));
        let mut counts = json!({"new": stats["new_count"].as_i64().unwrap_or(0), "learn": stats["learn_count"].as_i64().unwrap_or(0), "review": stats["review_count"].as_i64().unwrap_or(0)});
        let base = format!("{} -is:suspended -is:buried", q(&name));
        let ids = |v: Value| -> Vec<i64> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default() };
        let mut card_id: Option<i64> = None;
        if counts["learn"].as_i64().unwrap_or(0) > 0 {
            // "is:due" includes learning cards due within the learn-ahead window; only take ones due now.
            let found = ids(self.call("findCards", json!({"query": format!("{base} is:due is:learn")})).await?);
            if !found.is_empty() {
                let t = now();
                let infos: Vec<Value> = self.call("cardsInfo", json!({"cards": &found[..found.len().min(200)]})).await?.as_array().cloned().unwrap_or_default();
                card_id = infos
                    .iter()
                    .filter(|c| c["queue"] == 3 || c["due"].as_f64().unwrap_or(0.0) <= t)
                    .min_by(|a, b| {
                        let ka = (a["queue"] != 1, a["due"].as_f64().unwrap_or(0.0));
                        let kb = (b["queue"] != 1, b["due"].as_f64().unwrap_or(0.0));
                        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .and_then(|c| c["cardId"].as_i64());
            }
        }
        if card_id.is_none() && counts["review"].as_i64().unwrap_or(0) > 0 {
            card_id = ids(self.call("findCards", json!({"query": format!("{base} is:due is:review -is:learn")})).await?).first().copied();
        }
        if card_id.is_none() && counts["new"].as_i64().unwrap_or(0) > 0 {
            // card ids are creation times, so this is the oldest new card
            card_id = ids(self.call("findCards", json!({"query": format!("{base} is:new")})).await?).into_iter().min();
        }
        if card_id.is_none() {
            // Nothing else left: like Anki, show learning cards due within the next 20 minutes.
            let found = ids(self.call("findCards", json!({"query": format!("{base} is:learn")})).await?);
            if !found.is_empty() {
                let soon = now() + LEARN_AHEAD;
                let infos: Vec<Value> = self
                    .call("cardsInfo", json!({"cards": &found[..found.len().min(200)]}))
                    .await?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c["queue"] == 1 && c["due"].as_f64().unwrap_or(f64::MAX) <= soon)
                    .collect();
                if let Some(c) = infos.iter().min_by(|a, b| a["due"].as_f64().partial_cmp(&b["due"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)) {
                    card_id = c["cardId"].as_i64();
                    counts["learn"] = json!(counts["learn"].as_i64().unwrap_or(0).max(infos.len() as i64));
                }
            }
        }
        let mut result = json!({"deck": {"id": deck_id, "name": name}, "counts": counts, "card": null});
        if let Some(cid) = card_id {
            let info = self.call("cardsInfo", json!({"cards": [cid]})).await?.get(0).cloned().unwrap_or(json!({}));
            let queue = info["queue"].as_i64().unwrap_or(0);
            let kind = if queue == 1 || queue == 3 { "learn" } else if queue == 0 { "new" } else { "review" };
            result["card"] = json!({
                "id": info["cardId"], "ord": info["ord"], "kind": kind, "model": info["modelName"],
                "question": info["question"], "answer": info["answer"],
                "next": info.get("nextReviews").cloned().filter(crate::util::truthy).unwrap_or(json!([])),
            });
        }
        Ok(result)
    }

    pub async fn answer(&self, card_id: i64, ease: i64) -> Result<()> {
        if !EASES.contains(&ease) {
            return Err(Error::Invalid("ease must be 1-4".into()));
        }
        let ok = self.call("answerCards", json!({"answers": [{"cardId": card_id, "ease": ease}]})).await?;
        if !ok.get(0).map(crate::util::truthy).unwrap_or(false) {
            return Err(Error::NotFound("That card no longer exists in Anki".into()));
        }
        Ok(())
    }

    /// A media file from Anki's collection, with a safe content type.
    pub async fn media(&self, filename: &str) -> Result<Option<(Vec<u8>, String)>> {
        let data = self.call("retrieveMediaFile", json!({"filename": filename})).await?;
        let Some(b64) = data.as_str().filter(|d| !d.is_empty()) else { return Ok(None) };
        let mut ctype = guess_type(filename).to_string();
        if !(ctype.starts_with("image/") || ctype.starts_with("audio/") || ctype.starts_with("video/") || ctype.starts_with("font/")) || ctype.contains("svg") {
            ctype = "application/octet-stream".into();
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| Error::Anki(e.to_string()))?;
        Ok(Some((bytes, ctype)))
    }

    /// Open Anki's Add window with this deck selected.
    pub async fn add_cards(&self, deck_id: i64) -> Result<()> {
        let name = self.deck_name(deck_id).await?;
        match self.call("guiAddCards", json!({"note": {"deckName": name, "modelName": "Basic", "fields": {}}})).await {
            Err(Error::Anki(_)) => {
                self.call("guiAddCards", json!({})).await?; // no "Basic" note type: open it without presets
            }
            other => {
                other?;
            }
        }
        Ok(())
    }

    // --- cards from checkpoints (the PDF viewer's checkpoint mode) -------------------------------------
    /// Add a checkpoint's cards to its course's deck (or fallback_deck if the course has none).
    /// notes: [{front, back, hint, source}] as HTML; media: [{filename, data}] (base64 images the
    /// notes use: the passage shown as the hint, drawn molecules).
    pub async fn add_checkpoint_notes(&self, course_id: Option<i64>, fallback_deck: &str, notes: &[Value], media: &[Value]) -> Result<Value> {
        let model = checkpoint_note();
        let models = self.call("modelNames", json!({})).await?;
        if !models.as_array().map(|a| a.iter().any(|m| *m == model["modelName"])).unwrap_or(false) {
            self.call("createModel", model.clone()).await?;
        }
        let mut deck = fallback_deck.to_string();
        if let Some(did) = course_id.and_then(|c| self.course_decks().get(&c).copied()) {
            if let Ok(name) = self.deck_name(did).await {
                deck = name; // else the course's deck was deleted in Anki
            }
        }
        self.call("createDeck", json!({"deck": deck})).await?;
        for m in media {
            self.call("storeMediaFile", json!({"filename": m["filename"], "data": m["data"]})).await?;
        }
        let mut ids = Vec::new();
        for n in notes {
            let mut fields = Map::new();
            for (k, f) in [("Front", "front"), ("Back", "back"), ("Hint", "hint"), ("Source", "source")] {
                fields.insert(k.into(), n.get(f).cloned().unwrap_or(json!("")));
            }
            ids.push(
                self.call(
                    "addNote",
                    json!({"note": {
                        "deckName": deck, "modelName": model["modelName"], "tags": ["pdf-checkpoint"],
                        "fields": fields, "options": {"allowDuplicate": false},
                    }}),
                )
                .await?,
            );
        }
        Ok(json!({"deck": deck, "ids": ids}))
    }

    pub async fn sync(&self) -> Result<()> {
        self.call_timeout("sync", json!({}), Duration::from_secs(120)).await.map(|_| ())
    }

    /// Close Anki the way its window's close button does.
    pub async fn quit(&self) {
        let _ = self.call("guiExitAnki", json!({})).await; // offline or taken: stop() closes it instead
    }

    pub async fn wait_online(&self, seconds: f64) -> Result<bool> {
        for _ in 0..(seconds * 2.0) as i64 {
            match self.call_timeout("version", json!({}), Duration::from_secs(2)).await {
                Ok(_) => return Ok(true),
                Err(Error::AnkiOffline(_)) => tokio::time::sleep(Duration::from_millis(500)).await,
                Err(Error::AnkiPortTaken(u)) => return Err(Error::AnkiPortTaken(u)),
                Err(_) => return Ok(true), // AnkiConnect answered, with an error
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deck_query() {
        assert_eq!(super::q("Canvas::CSE_373"), "\"deck:Canvas::CSE\\_373\"");
        assert_eq!(super::q("a\"b*"), "\"deck:a\\\"b\\*\"");
    }
}
