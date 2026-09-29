//! Stale-while-revalidate cache over Canvas resources, a background sync, and change notifications.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::{Map, Value, json};
use tokio::sync::{Semaphore, broadcast};

use crate::client::Client;
use crate::store::{Store, now};
use crate::{Error, Result, resources};

pub const SYNC_EVERY: Duration = Duration::from_secs(5 * 60);
pub const CONCURRENCY: usize = 6;

pub fn make_key(name: &str, args: &[String]) -> String {
    std::iter::once(name.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(":")
}

/// What listeners hear: the status changed, a cached resource changed, or a notebook job moved.
#[derive(Clone, Debug)]
pub enum Event {
    Status(Value),
    Changed(String),
    Job(Value),
}

impl Event {
    pub fn to_json(&self) -> Value {
        match self {
            Event::Status(s) => json!({"status": s}),
            Event::Changed(k) => json!({"changed": k}),
            Event::Job(j) => json!({"job": j}),
        }
    }
}

/// Resources beyond the built-in Canvas ones (recordings, notebooks).
pub trait ResourceExt: Send + Sync {
    fn ttl(&self, name: &str) -> Option<f64>;
    fn fetch(&self, name: &str, args: Vec<String>) -> BoxFuture<'static, Result<Value>>;
}

type Hook = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
type Inflight = Shared<BoxFuture<'static, Result<()>>>;

pub struct Engine {
    pub store: Store,
    pub client: Client,
    sem: Semaphore,
    inflight: Mutex<HashMap<String, Inflight>>,
    events: broadcast::Sender<Event>,
    status: Mutex<Map<String, Value>>,
    ext: OnceLock<Arc<dyn ResourceExt>>,
    after_sync: Mutex<Vec<Hook>>,
}

impl Engine {
    pub fn new(store: Store, client: Client) -> Arc<Engine> {
        let (events, _) = broadcast::channel(1024);
        let status = json!({"syncing": false, "last_sync": null, "session": "unknown", "error": null});
        Arc::new(Engine {
            store,
            client,
            sem: Semaphore::new(CONCURRENCY),
            inflight: Mutex::new(HashMap::new()),
            events,
            status: Mutex::new(status.as_object().unwrap().clone()),
            ext: OnceLock::new(),
            after_sync: Mutex::new(Vec::new()),
        })
    }

    pub fn set_ext(&self, ext: Arc<dyn ResourceExt>) {
        let _ = self.ext.set(ext);
    }

    pub fn add_after_sync(&self, hook: Hook) {
        self.after_sync.lock().unwrap().push(hook);
    }

    // --- notifications -------------------------------------------------
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn broadcast(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub fn status(&self) -> Value {
        Value::Object(self.status.lock().unwrap().clone())
    }

    /// Change status fields; tells listeners if anything actually changed.
    pub fn set_status(&self, kw: Vec<(&str, Value)>) {
        let snapshot = {
            let mut st = self.status.lock().unwrap();
            let changed = kw.iter().any(|(k, v)| st.get(*k) != Some(v));
            if !changed {
                return;
            }
            for (k, v) in kw {
                st.insert(k.to_string(), v);
            }
            Value::Object(st.clone())
        };
        self.broadcast(Event::Status(snapshot));
    }

    /// Set status fields without telling anyone (initial values).
    pub fn init_status(&self, kw: Vec<(&str, Value)>) {
        let mut st = self.status.lock().unwrap();
        for (k, v) in kw {
            st.insert(k.to_string(), v);
        }
    }

    pub fn ttl(&self, name: &str) -> Option<f64> {
        resources::ttl(name).or_else(|| self.ext.get().and_then(|e| e.ttl(name)))
    }

    // --- reads ---------------------------------------------------------
    /// Cached JSON text + fetched_at. Stale or forced data is refreshed in the background;
    /// only a cold miss waits on the network.
    pub async fn get(self: &Arc<Self>, name: &str, args: &[String], force: bool) -> Result<(String, f64)> {
        let Some(ttl) = self.ttl(name) else { return Err(Error::NotFound("unknown resource".into())) };
        let key = make_key(name, args);
        match self.store.get(&key) {
            None => {
                self.refresh(name, args).await?;
                self.store.get(&key).ok_or(Error::NotFound(key))
            }
            Some(row) => {
                if force || now() - row.1 > ttl {
                    self.refresh_bg(name, args);
                }
                Ok(row)
            }
        }
    }

    /// Parsed get().
    pub async fn get_json(self: &Arc<Self>, name: &str, args: &[String]) -> Result<Value> {
        let (text, _) = self.get(name, args, false).await?;
        Ok(serde_json::from_str(&text)?)
    }

    /// For callers that want current data (the MCP server): the cached copy if it's within its
    /// TTL; otherwise refresh, waiting up to `wait`. If that fails or times out, fall back to the
    /// stale copy. Returns (data, fetched_at, why_stale). A cold miss waits for the network.
    pub async fn read(self: &Arc<Self>, name: &str, args: &[String], wait: Duration) -> Result<(Value, f64, Option<String>)> {
        let Some(ttl) = self.ttl(name) else { return Err(Error::NotFound(format!("unknown resource {name}"))) };
        let key = make_key(name, args);
        let row = self.store.get(&key);
        if let Some((text, at)) = &row {
            if now() - at <= ttl {
                return Ok((serde_json::from_str(text)?, *at, None));
            }
        }
        match &row {
            None => self.refresh(name, args).await?,
            Some((text, at)) => {
                // A timeout leaves the refresh running.
                let why = match tokio::time::timeout(wait, self.refresh(name, args)).await {
                    Err(_) => Some("Canvas didn't respond in time".to_string()),
                    Ok(Err(e)) if e.is_session() => Some("the Canvas session expired (log into Canvas in Firefox)".to_string()),
                    Ok(Err(e)) => Some(format!("Canvas couldn't be reached ({})", e.type_name())),
                    Ok(Ok(())) => None,
                };
                if let Some(why) = why {
                    return Ok((serde_json::from_str(text)?, *at, Some(why)));
                }
            }
        }
        let (text, at) = self.store.get(&key).ok_or(Error::NotFound(key))?;
        Ok((serde_json::from_str(&text)?, at, None))
    }

    fn start(self: &Arc<Self>, name: &str, args: &[String]) -> Inflight {
        let key = make_key(name, args);
        let mut inflight = self.inflight.lock().unwrap();
        if let Some(f) = inflight.get(&key) {
            return f.clone();
        }
        let this = self.clone();
        let (name, args, k) = (name.to_string(), args.to_vec(), key.clone());
        let (tx, rx) = futures::channel::oneshot::channel();
        tokio::spawn(async move {
            let _ = tx.send(this.do_refresh(&name, &args, &k).await);
        });
        let fut: BoxFuture<'static, Result<()>> = async move { rx.await.unwrap_or_else(|_| Err(Error::Other("the refresh was cancelled".into()))) }.boxed();
        let shared = fut.shared();
        inflight.insert(key, shared.clone());
        shared
    }

    pub fn refresh_bg(self: &Arc<Self>, name: &str, args: &[String]) {
        let _ = self.start(name, args); // errors are logged and broadcast by do_refresh
    }

    pub async fn refresh(self: &Arc<Self>, name: &str, args: &[String]) -> Result<()> {
        self.start(name, args).await
    }

    async fn fetch(self: &Arc<Self>, name: &str, args: &[String]) -> Result<Value> {
        if resources::ttl(name).is_some() {
            return resources::fetch(&self.client, name, args).await;
        }
        match self.ext.get() {
            Some(ext) => ext.fetch(name, args.to_vec()).await,
            None => Err(Error::NotFound(format!("unknown resource {name}"))),
        }
    }

    async fn do_refresh(self: &Arc<Self>, name: &str, args: &[String], key: &str) -> Result<()> {
        let result = async {
            let data = {
                let _permit = self.sem.acquire().await.map_err(|e| Error::Other(e.to_string()))?;
                self.fetch(name, args).await
            };
            match data {
                Ok(data) => {
                    self.set_status(vec![("session", json!("ok")), ("error", Value::Null)]);
                    let text = serde_json::to_string(&data)?;
                    if self.store.put(key, &text) {
                        self.broadcast(Event::Changed(key.to_string()));
                    }
                    Ok(())
                }
                Err(e) if e.is_session() => {
                    let session = if matches!(e, Error::NeedsPermission(_)) { "permission" } else { "expired" };
                    self.set_status(vec![("session", json!(session)), ("error", json!(e.to_string()))]);
                    Err(e)
                }
                Err(e) => {
                    log::warn!("refresh {key} failed: {e}");
                    Err(e)
                }
            }
        }
        .await;
        self.inflight.lock().unwrap().remove(key);
        result
    }

    pub fn cached(&self, name: &str, args: &[String]) -> Option<Value> {
        let (text, _) = self.store.get(&make_key(name, args))?;
        serde_json::from_str(&text).ok()
    }

    /// Cached list (or empty).
    pub fn cached_list(&self, name: &str, args: &[String]) -> Vec<Value> {
        match self.cached(name, args) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        }
    }

    /// Refresh if stale (waiting for it), swallowing errors other than session ones; used by the sync.
    pub async fn fresh(self: &Arc<Self>, name: &str, args: &[String]) -> Result<Option<Value>> {
        let ttl = self.ttl(name).unwrap_or(0.0);
        let stale = self.store.get(&make_key(name, args)).map(|(_, at)| now() - at > ttl).unwrap_or(true);
        if stale {
            match self.refresh(name, args).await {
                Err(e) if e.is_session() => return Err(e),
                _ => {}
            }
        }
        Ok(self.cached(name, args))
    }

    // --- background sync -------------------------------------------------
    pub async fn sync_once(self: &Arc<Self>) {
        if self.client.base().is_empty() {
            return; // not set up yet; the app's setup screen starts a sync when it is
        }
        self.set_status(vec![("syncing", json!(true))]);
        let result: Result<()> = async {
            let top = ["self", "colors", "planner", "announcements", "inbox", "past_courses"];
            for r in futures::future::join_all(top.iter().map(|n| self.fresh(n, &[]))).await {
                r?;
            }
            let courses = match self.fresh("courses", &[]).await? {
                Some(Value::Array(a)) => a,
                _ => Vec::new(),
            };
            // Past courses aren't synced; their content loads on first open and stays cached.
            let per_course = ["groups", "modules", "pages", "files", "discussions", "syllabus", "tabs", "course_announcements"];
            let ids: Vec<String> = courses.iter().map(|c| c["id"].to_string()).collect();
            let jobs: Vec<_> = ids.iter().flat_map(|cid| per_course.iter().map(move |n| (n, cid.clone()))).collect();
            for r in futures::future::join_all(jobs.iter().map(|(n, cid)| self.fresh(n, std::slice::from_ref(cid)))).await {
                r?;
            }
            // Deep: page bodies whose list entry changed since we cached them.
            let mut deep = Vec::new();
            for cid in &ids {
                for p in self.cached_list("pages", std::slice::from_ref(cid)) {
                    let Some(url) = p["url"].as_str() else { continue };
                    let args = vec![cid.clone(), url.to_string()];
                    let body = self.cached("page", &args);
                    if body.as_ref().map(|b| b.get("updated_at") != p.get("updated_at")).unwrap_or(true) {
                        deep.push(args);
                    }
                }
            }
            futures::future::join_all(deep.iter().map(|a| self.refresh("page", a))).await;
            self.set_status(vec![("last_sync", json!(now()))]);
            Ok(())
        }
        .await;
        let _ = result; // a session problem is already in the status
        self.set_status(vec![("syncing", json!(false))]);
    }

    pub async fn sync_loop(self: Arc<Self>) {
        loop {
            self.sync_once().await;
            let hooks: Vec<Hook> = self.after_sync.lock().unwrap().clone();
            for hook in hooks {
                hook().await;
            }
            tokio::time::sleep(SYNC_EVERY).await;
        }
    }
}
