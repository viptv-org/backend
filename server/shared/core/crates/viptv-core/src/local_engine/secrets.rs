use crate::CoreError;
use std::sync::Arc;

#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    fn get(&self, key: String) -> Result<Option<String>, CoreError>;
    fn put(&self, key: String, value: String) -> Result<(), CoreError>;
    fn remove(&self, key: String) -> Result<(), CoreError>;
}

#[cfg(all(feature = "os-secrets", not(target_os = "android")))]
struct SystemSecrets;

#[cfg(all(feature = "os-secrets", not(any(target_os = "android", target_os = "linux"))))]
impl SecretStore for SystemSecrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        let entry =
            keyring::Entry::new("org.viptv.experiment", &key).map_err(|_| CoreError::Bridge)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CoreError::Bridge),
        }
    }
    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        keyring::Entry::new("org.viptv.experiment", &key)
            .map_err(|_| CoreError::Bridge)?
            .set_password(&value)
            .map_err(|_| CoreError::Bridge)
    }
    fn remove(&self, key: String) -> Result<(), CoreError> {
        let result = keyring::Entry::new("org.viptv.experiment", &key)
            .map_err(|_| CoreError::Bridge)?
            .delete_credential();
        match result {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CoreError::Bridge),
        }
    }
}

#[cfg(all(feature = "os-secrets", target_os = "linux"))]
mod linux;

#[cfg(not(all(feature = "os-secrets", not(target_os = "android"))))]
struct UnavailableSecrets;

#[cfg(not(all(feature = "os-secrets", not(target_os = "android"))))]
impl SecretStore for UnavailableSecrets {
    fn get(&self, _key: String) -> Result<Option<String>, CoreError> {
        Err(CoreError::Bridge)
    }
    fn put(&self, _key: String, _value: String) -> Result<(), CoreError> {
        Err(CoreError::Bridge)
    }
    fn remove(&self, _key: String) -> Result<(), CoreError> {
        Err(CoreError::Bridge)
    }
}

#[uniffi::export]
pub fn system_secret_store() -> Arc<dyn SecretStore> {
    #[cfg(all(feature = "os-secrets", not(target_os = "android")))]
    {
        Arc::new(SystemSecrets)
    }
    #[cfg(not(all(feature = "os-secrets", not(target_os = "android"))))]
    {
        Arc::new(UnavailableSecrets)
    }
}
