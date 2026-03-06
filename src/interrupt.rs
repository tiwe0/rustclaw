use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type CancelMap = HashMap<String, Arc<AtomicBool>>;

fn registry() -> &'static Mutex<CancelMap> {
    static REGISTRY: OnceLock<Mutex<CancelMap>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn session_token(session_id: &str) -> Arc<AtomicBool> {
    let key = session_id.trim();
    if key.is_empty() {
        return Arc::new(AtomicBool::new(false));
    }

    let mut locked = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let token = locked
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone();
    token.store(false, Ordering::Release);
    token
}

pub fn cancel_session(session_id: &str) {
    let key = session_id.trim();
    if key.is_empty() {
        return;
    }

    let mut locked = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let token = locked
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone();
    token.store(true, Ordering::Release);
}

pub fn clear_session(session_id: &str) {
    let key = session_id.trim();
    if key.is_empty() {
        return;
    }

    let mut locked = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    locked.remove(key);
}

pub fn is_cancelled(session_id: &str) -> bool {
    let key = session_id.trim();
    if key.is_empty() {
        return false;
    }

    let locked = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    locked
        .get(key)
        .map(|token| token.load(Ordering::Acquire))
        .unwrap_or(false)
}
