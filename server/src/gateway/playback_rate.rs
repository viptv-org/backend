use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const TRACKED_KEYS: usize = 4096;

pub(crate) struct Limiter {
    windows: Mutex<HashMap<String, (Instant, u32)>>,
    limit: u32,
    window: Duration,
}

impl Limiter {
    pub(crate) fn new(limit: u32, window: Duration) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            limit,
            window,
        }
    }

    pub(crate) fn admit(&self, key: &str, now: Instant) -> bool {
        let mut windows = self.windows.lock().unwrap();
        if windows.len() >= TRACKED_KEYS && !windows.contains_key(key) {
            windows.retain(|_, (start, _)| now.duration_since(*start) < self.window);
            if windows.len() >= TRACKED_KEYS {
                return false;
            }
        }
        let (start, count) = windows.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(*start) >= self.window {
            *start = now;
            *count = 0;
        }
        *count += 1;
        *count <= self.limit
    }
}
