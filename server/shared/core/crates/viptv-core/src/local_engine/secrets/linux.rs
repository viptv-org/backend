//! Linux vault access must never wait for an interactive unlock prompt.
//!
//! Startup cancellation cannot interrupt a synchronous Secret Service call across
//! UniFFI. Use the same credential identity as keyring 3.6, but do not unlock
//! items/collections and disallow any prompts returned by create/delete methods.
use super::{SecretStore, SystemSecrets};
use crate::CoreError;
use dbus_secret_service::{EncryptionType, Item, SecretService};
use std::collections::HashMap;

const SERVICE: &str = "org.viptv.experiment";
const TARGET: &str = "default";

fn connect() -> Result<SecretService, CoreError> {
    SecretService::connect_with_max_prompt_timeout(EncryptionType::Dh, 0)
        .map_err(|_| CoreError::Bridge)
}

fn unique<T>(mut items: Vec<T>) -> Result<Option<T>, CoreError> {
    match items.len() {
        0 => Ok(None),
        1 => Ok(items.pop()),
        _ => Err(CoreError::Bridge),
    }
}

fn resolve_matches<T>(
    unlocked: Vec<T>,
    locked_count: usize,
    legacy: impl FnOnce() -> Result<Vec<T>, CoreError>,
) -> Result<Option<T>, CoreError> {
    if locked_count != 0 {
        return Err(CoreError::Bridge);
    }
    if unlocked.is_empty() {
        unique(legacy()?)
    } else {
        unique(unlocked)
    }
}

fn find<'a>(service: &'a SecretService, key: &str) -> Result<Option<Item<'a>>, CoreError> {
    let attributes = HashMap::from([("service", SERVICE), ("username", key), ("target", TARGET)]);
    let matches = service
        .search_items(attributes)
        .map_err(|_| CoreError::Bridge)?;
    let item = resolve_matches(matches.unlocked, matches.locked.len(), || {
        // Preserve keyring's v1 compatibility: only after no exact match, search
        // the default collection by service+username without a target attribute.
        let collection = service
            .get_default_collection()
            .map_err(|_| CoreError::Bridge)?;
        let items = collection
            .search_items(HashMap::from([("service", SERVICE), ("username", key)]))
            .map_err(|_| CoreError::Bridge)?;
        Ok(items
            .into_iter()
            .map(|item| Item::new(service, item.path))
            .collect())
    })?;
    if let Some(item) = &item {
        if item.is_locked().map_err(|_| CoreError::Bridge)? {
            return Err(CoreError::Bridge);
        }
    }
    Ok(item)
}

impl SecretStore for SystemSecrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        let service = connect()?;
        find(&service, &key)?
            .map(|item| {
                let bytes = item.get_secret().map_err(|_| CoreError::Bridge)?;
                String::from_utf8(bytes).map_err(|_| CoreError::Bridge)
            })
            .transpose()
    }

    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        let service = connect()?;
        if let Some(item) = find(&service, &key)? {
            return item
                .set_secret(value.as_bytes(), "text/plain")
                .map_err(|_| CoreError::Bridge);
        }
        let collection = service
            .get_default_collection()
            .map_err(|_| CoreError::Bridge)?;
        if collection.is_locked().map_err(|_| CoreError::Bridge)? {
            return Err(CoreError::Bridge);
        }
        collection
            .create_item(
                &format!("{key}@{SERVICE}:{TARGET} (keyring v3.6.3)"),
                HashMap::from([
                    ("service", SERVICE),
                    ("username", key.as_str()),
                    ("target", TARGET),
                    ("application", "rust-keyring"),
                ]),
                value.as_bytes(),
                true,
                "text/plain",
            )
            .map_err(|_| CoreError::Bridge)?;
        Ok(())
    }

    fn remove(&self, key: String) -> Result<(), CoreError> {
        let service = connect()?;
        if let Some(item) = find(&service, &key)? {
            item.delete().map_err(|_| CoreError::Bridge)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_credential_does_not_fall_back_to_an_unrelated_legacy_match() {
        assert_eq!(
            resolve_matches(vec!["current"], 0, || panic!("legacy lookup")).unwrap(),
            Some("current")
        );
    }

    #[test]
    fn ambiguous_exact_credentials_are_not_resolved_by_legacy_fallback() {
        assert!(matches!(
            resolve_matches(vec![1, 2], 0, || panic!("legacy lookup")),
            Err(CoreError::Bridge)
        ));
    }

    #[test]
    fn locked_credential_is_an_error_even_when_an_unlocked_match_exists() {
        assert!(matches!(
            resolve_matches(vec![1], 1, || panic!("legacy lookup")),
            Err(CoreError::Bridge)
        ));
    }

    #[test]
    fn missing_exact_credential_preserves_legacy_and_absent_results() {
        assert_eq!(
            resolve_matches(Vec::<i32>::new(), 0, || Ok(vec![7])).unwrap(),
            Some(7)
        );
        assert_eq!(
            resolve_matches(Vec::<i32>::new(), 0, || Ok(vec![])).unwrap(),
            None
        );
    }

    #[test]
    fn ambiguous_or_unavailable_legacy_vault_is_not_treated_as_empty() {
        assert!(matches!(
            resolve_matches(Vec::<i32>::new(), 0, || Ok(vec![1, 2])),
            Err(CoreError::Bridge)
        ));
        assert!(matches!(
            resolve_matches(Vec::<i32>::new(), 0, || Err(CoreError::Bridge)),
            Err(CoreError::Bridge)
        ));
    }
}
