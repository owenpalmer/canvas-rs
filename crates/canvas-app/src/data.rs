//! The UI's view of the cache: an in-memory memo of resources, filled in the background from the
//! engine (which serves its SQLite cache instantly and refreshes stale data behind the scenes).
//!
//! Views ask with need(): the data if it's here, else Pending (a fetch starts) or the error. A view
//! gathers everything it needs before drawing anything, so a Pending never leaves half a view on
//! screen. lazy() is for parts that may arrive later (replies, a submission): the view draws without
//! them and redraws when they come. Every key a view asks for is recorded, so a change to any of
//! them (from the background sync, over the engine's events) redraws it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use canvas_mcp::services::{ApiErr, Services};
use serde_json::Value;

pub type Json = Arc<Value>;

pub fn key_of(name: &str, args: &[String]) -> String {
    std::iter::once(name.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(":")
}

/// Why a view can't be drawn yet.
#[derive(Clone, Debug)]
pub enum Need {
    Pending,
    Err(ApiErr),
}

impl From<ApiErr> for Need {
    fn from(e: ApiErr) -> Self {
        Need::Err(e)
    }
}

pub type Got<T> = Result<T, Need>;

/// A lazily loaded part.
pub enum Lazy {
    Ready(Json),
    Pending,
    Failed,
}

impl Lazy {
    pub fn ready(&self) -> Option<&Json> {
        match self {
            Lazy::Ready(v) => Some(v),
            _ => None,
        }
    }
}

/// A result coming back to the UI thread.
pub enum Loaded {
    Resource { key: String, result: Result<(Value, f64), ApiErr>, revalidate: bool },
}

pub struct Data {
    pub svc: Arc<Services>,
    rt: tokio::runtime::Handle,
    tx: std::sync::mpsc::Sender<crate::app::Msg>,
    memo: HashMap<String, Json>,
    fetched_at: HashMap<String, f64>,
    errors: HashMap<String, ApiErr>,
    pending: RefCell<HashSet<String>>,
    touched: RefCell<HashSet<String>>,
    /// Bumped whenever anything in the memo changes.
    pub generation: u64,
    /// Network fetches started (for the debug panel's "n fetched").
    pub fetches: std::cell::Cell<u64>,
}

fn split_key(key: &str) -> (String, Vec<String>) {
    // Page slugs can't contain ':' (Canvas makes them url-safe), so splitting is safe.
    let mut parts = key.split(':').map(String::from);
    let name = parts.next().unwrap_or_default();
    (name, parts.collect())
}

impl Data {
    pub fn new(svc: Arc<Services>, rt: tokio::runtime::Handle, tx: std::sync::mpsc::Sender<crate::app::Msg>, boot: HashMap<String, Value>) -> Data {
        Data {
            svc,
            rt,
            tx,
            memo: boot.into_iter().map(|(k, v)| (k, Arc::new(v))).collect(),
            fetched_at: HashMap::new(),
            errors: HashMap::new(),
            pending: RefCell::new(HashSet::new()),
            touched: RefCell::new(HashSet::new()),
            generation: 0,
            fetches: std::cell::Cell::new(0),
        }
    }

    fn fetch(&self, key: &str, force: bool, revalidate: bool) {
        if !revalidate && !self.pending.borrow_mut().insert(key.to_string()) {
            return;
        }
        self.fetches.set(self.fetches.get() + 1);
        let (name, args) = split_key(key);
        let (svc, tx, key) = (self.svc.clone(), self.tx.clone(), key.to_string());
        self.rt.spawn(async move {
            let result = svc.resource(&name, &args, force).await;
            let _ = tx.send(crate::app::Msg::Data(Loaded::Resource { key, result, revalidate }));
            crate::app::wake();
        });
    }

    /// The data, or Pending (and a fetch starts), or the error from the last try.
    pub fn need(&self, name: &str, args: &[String]) -> Got<Json> {
        let key = key_of(name, args);
        self.touched.borrow_mut().insert(key.clone());
        if let Some(v) = self.memo.get(&key) {
            return Ok(v.clone());
        }
        if let Some(e) = self.errors.get(&key) {
            return Err(Need::Err(e.clone()));
        }
        self.fetch(&key, false, false);
        Err(Need::Pending)
    }

    pub fn need0(&self, name: &str) -> Got<Json> {
        self.need(name, &[])
    }

    /// need() with one argument.
    pub fn need1(&self, name: &str, arg: impl ToString) -> Got<Json> {
        self.need(name, &[arg.to_string()])
    }

    /// The data if it's here (fetching it in the background if not).
    pub fn lazy(&self, name: &str, args: &[String]) -> Lazy {
        match self.need(name, args) {
            Ok(v) => Lazy::Ready(v),
            Err(Need::Pending) => Lazy::Pending,
            Err(Need::Err(_)) => Lazy::Failed,
        }
    }

    /// Cached without asking (no fetch, not a dependency).
    pub fn peek(&self, key: &str) -> Option<Json> {
        self.memo.get(key).cloned()
    }

    pub fn has(&self, key: &str) -> bool {
        self.memo.contains_key(key)
    }

    /// Start recording which keys a view reads; returns the previous set.
    pub fn begin_deps(&self) -> HashSet<String> {
        std::mem::take(&mut *self.touched.borrow_mut())
    }

    /// The keys read since begin_deps(), restoring `outer` for nested recording.
    pub fn end_deps(&self, outer: HashSet<String>) -> HashSet<String> {
        std::mem::replace(&mut *self.touched.borrow_mut(), outer)
    }

    /// Ask the engine about each key again, so stale ones refresh in the background (changes
    /// come back as events); `force` refreshes even fresh ones.
    pub fn revalidate<'a>(&self, keys: impl IntoIterator<Item = &'a String>, force: bool) {
        for k in keys {
            self.fetch(k, force, true);
        }
    }

    pub fn forget(&mut self, key: &str) {
        if self.memo.remove(key).is_some() {
            self.generation += 1;
        }
        self.errors.remove(key);
    }

    pub fn forget_prefix(&mut self, prefix: &str) {
        let keys: Vec<String> = self.memo.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
        for k in keys {
            self.forget(&k);
        }
        self.errors.retain(|k, _| !k.starts_with(prefix));
    }

    pub fn clear_all(&mut self) {
        self.memo.clear();
        self.errors.clear();
        self.generation += 1;
    }

    /// Errors are remembered until something happens (so a failing load isn't retried every frame).
    pub fn clear_errors(&mut self) {
        if !self.errors.is_empty() {
            self.errors.clear();
            self.generation += 1;
        }
    }

    /// A resource changed in the cache: fetch it again, keeping the old copy on screen meanwhile.
    pub fn changed(&mut self, key: &str) {
        self.errors.remove(key);
        if self.memo.contains_key(key) {
            self.fetch(key, false, true);
        }
    }

    /// Take a result from the background. Returns true if the memo changed.
    pub fn loaded(&mut self, l: Loaded) -> bool {
        match l {
            Loaded::Resource { key, result, revalidate } => {
                if !revalidate {
                    self.pending.borrow_mut().remove(&key);
                }
                match result {
                    Ok((v, at)) => {
                        self.fetched_at.insert(key.clone(), at);
                        self.errors.remove(&key);
                        if self.memo.get(&key).map(|old| **old == v).unwrap_or(false) {
                            return false;
                        }
                        self.memo.insert(key, Arc::new(v));
                        self.generation += 1;
                        true
                    }
                    Err(e) => {
                        if revalidate {
                            return false; // keep what's shown
                        }
                        self.errors.insert(key, e);
                        self.generation += 1;
                        true
                    }
                }
            }
        }
    }
}
