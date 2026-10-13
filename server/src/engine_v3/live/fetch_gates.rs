use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
#[derive(Default)]
pub(crate) struct FetchGates(Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>);
impl FetchGates {
    pub(crate) fn connection(
        &self,
        id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, crate::ApiError> {
        let mut gates = self
            .0
            .lock()
            .map_err(|_| crate::ApiError::from("Live data unavailable"))?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(id).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        if gates.len() >= 128 {
            return Err("Live refresh capacity reached".into());
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        gates.insert(id.into(), Arc::downgrade(&gate));
        Ok(gate)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn per_connection_coalescing_keeps_other_directories_independent() {
        let gates = FetchGates::default();
        let a = gates.connection("m3u:1").unwrap();
        let hold = a.lock().await;
        let b = gates.connection("m3u:2").unwrap();
        assert!(b.try_lock().is_ok());
        let same = gates.connection("m3u:1").unwrap();
        assert!(Arc::ptr_eq(&a, &same));
        assert!(same.try_lock().is_err());
        drop(hold);
        assert!(same.try_lock().is_ok());
    }
}
