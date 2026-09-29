//! Authenticated encryption for account-owned source/gateway credentials.
//! Key material is operator supplied, never generated into the database.
use base64::{engine::general_purpose::STANDARD, Engine};
use rand_core::{OsRng, RngCore};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use zeroize::{Zeroize, Zeroizing};

type Result<T> = std::result::Result<T, &'static str>;
const MAX_SECRET: usize = 256 * 1024;

pub(crate) struct SecretBytes(Zeroizing<Vec<u8>>);
impl SecretBytes {
    pub(crate) fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sealed {
    version: u8,
    key_id: String,
    nonce: String,
    ciphertext: String,
}
impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sealed(<redacted>)")
    }
}
pub(crate) struct Vault {
    active: String,
    keys: BTreeMap<String, LessSafeKey>,
}
impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Vault(<redacted>)")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    active: String,
    #[serde(deserialize_with = "key_map")]
    keys: BTreeMap<String, Zeroizing<String>>,
}
fn key_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Zeroizing<String>>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Zeroizing<String>>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("unique encryption key IDs")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut input: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((id, value)) = input.next_entry::<String, String>()? {
                if result.insert(id, Zeroizing::new(value)).is_some() {
                    return Err(serde::de::Error::custom("duplicate encryption key ID"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}
impl Vault {
    pub(crate) fn from_environment() -> Result<Option<Self>> {
        match std::env::var("VIPTV_SECRETS_KEYRING") {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("invalid_secret_keyring"),
            Ok(value) => Self::from_json(&Zeroizing::new(value)).map(Some),
        }
    }
    pub(crate) fn from_json(value: &str) -> Result<Self> {
        if value.len() > 8192 {
            return Err("invalid_secret_keyring");
        }
        let config: Config = serde_json::from_str(value).map_err(|_| "invalid_secret_keyring")?;
        if config.keys.is_empty()
            || config.keys.len() > 8
            || !config.keys.contains_key(&config.active)
        {
            return Err("invalid_secret_keyring");
        }
        let mut keys = BTreeMap::new();
        for (id, mut encoded) in config.keys {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err("invalid_secret_keyring");
            }
            let key = Zeroizing::new(
                STANDARD
                    .decode(encoded.as_bytes())
                    .map_err(|_| "invalid_secret_keyring")?,
            );
            encoded.zeroize();
            let key =
                UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| "invalid_secret_keyring")?;
            keys.insert(id, LessSafeKey::new(key));
        }
        Ok(Self {
            active: config.active,
            keys,
        })
    }
    fn context(account: i64, purpose: &str, record: &str, key: &str) -> Result<Vec<u8>> {
        if account <= 0
            || purpose.is_empty()
            || purpose.len() > 64
            || record.is_empty()
            || record.len() > 128
        {
            return Err("invalid_secret_context");
        }
        serde_json::to_vec(&serde_json::json!([
            "viptv-secret",
            1,
            key,
            account,
            purpose,
            record
        ]))
        .map_err(|_| "invalid_secret_context")
    }
    pub(crate) fn seal(
        &self,
        account: i64,
        purpose: &str,
        record: &str,
        plaintext: &[u8],
    ) -> Result<String> {
        if plaintext.len() > MAX_SECRET {
            return Err("secret_too_large");
        }
        let aad = Self::context(account, purpose, record, &self.active)?;
        let mut nonce = [0u8; 12];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| "secret_entropy_unavailable")?;
        let mut bytes = Zeroizing::new(plaintext.to_vec());
        self.keys
            .get(&self.active)
            .ok_or("secret_key_unavailable")?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut *bytes,
            )
            .map_err(|_| "secret_encryption_failed")?;
        serde_json::to_string(&Sealed {
            version: 1,
            key_id: self.active.clone(),
            nonce: STANDARD.encode(nonce),
            ciphertext: STANDARD.encode(&*bytes),
        })
        .map_err(|_| "secret_encryption_failed")
    }
    pub(crate) fn open(
        &self,
        account: i64,
        purpose: &str,
        record: &str,
        envelope: &str,
    ) -> Result<SecretBytes> {
        if envelope.len() > 400000 {
            return Err("invalid_secret_envelope");
        }
        let sealed: Sealed =
            serde_json::from_str(envelope).map_err(|_| "invalid_secret_envelope")?;
        if sealed.version != 1 {
            return Err("invalid_secret_envelope");
        }
        let key = self
            .keys
            .get(&sealed.key_id)
            .ok_or("secret_key_unavailable")?;
        let nonce: [u8; 12] = STANDARD
            .decode(&sealed.nonce)
            .map_err(|_| "invalid_secret_envelope")?
            .try_into()
            .map_err(|_| "invalid_secret_envelope")?;
        let aad = Self::context(account, purpose, record, &sealed.key_id)?;
        let mut bytes = Zeroizing::new(
            STANDARD
                .decode(&sealed.ciphertext)
                .map_err(|_| "invalid_secret_envelope")?,
        );
        if bytes.len() > MAX_SECRET + aead::AES_256_GCM.tag_len() {
            return Err("invalid_secret_envelope");
        }
        let length = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut bytes,
            )
            .map_err(|_| "secret_authentication_failed")?
            .len();
        bytes.truncate(length);
        Ok(SecretBytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keyring(active: &str, first: u8, second: Option<u8>) -> Vault {
        let mut keys = serde_json::json!({"first":STANDARD.encode([first;32])});
        if let Some(value) = second {
            keys["second"] = serde_json::json!(STANDARD.encode([value; 32]));
        }
        Vault::from_json(&serde_json::json!({"active":active,"keys":keys}).to_string()).unwrap()
    }
    #[test]
    fn ciphertext_is_randomized_authenticated_and_bound_to_account_purpose_record() {
        let vault = keyring("first", 7, None);
        let one = vault
            .seal(1, "gateway", "record-one", b"private-source-password")
            .unwrap();
        let two = vault
            .seal(1, "gateway", "record-one", b"private-source-password")
            .unwrap();
        assert_ne!(one, two);
        assert!(!one.contains("private-source-password"));
        let plain = vault.open(1, "gateway", "record-one", &one).unwrap();
        assert_eq!(plain.expose(), b"private-source-password");
        assert_eq!(format!("{plain:?}"), "SecretBytes(<redacted>)");
        assert!(vault.open(2, "gateway", "record-one", &one).is_err());
        assert!(vault.open(1, "provider", "record-one", &one).is_err());
        assert!(vault.open(1, "gateway", "record-two", &one).is_err());
        let mut edited: Sealed = serde_json::from_str(&one).unwrap();
        let mut ciphertext = STANDARD.decode(&edited.ciphertext).unwrap();
        ciphertext[0] ^= 1;
        edited.ciphertext = STANDARD.encode(ciphertext);
        assert!(vault
            .open(
                1,
                "gateway",
                "record-one",
                &serde_json::to_string(&edited).unwrap()
            )
            .is_err());
        assert!(keyring("first", 8, None)
            .open(1, "gateway", "record-one", &one)
            .is_err());
    }
    #[test]
    fn keyring_rotation_reads_old_ciphertexts_without_plaintext_fallback() {
        let old = keyring("first", 7, None)
            .seal(1, "provider", "one", b"private")
            .unwrap();
        let rotated = keyring("second", 7, Some(8));
        assert_eq!(
            rotated.open(1, "provider", "one", &old).unwrap().expose(),
            b"private"
        );
        let new = rotated.seal(1, "provider", "one", b"private").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&new).unwrap()["key_id"],
            "second"
        );
        assert!(keyring("first", 7, None)
            .open(1, "provider", "one", &new)
            .is_err());
        assert!(rotated.open(1, "provider", "one", "private").is_err());
        let encoded = STANDARD.encode([7u8; 32]);
        assert!(Vault::from_json(&format!(
            r#"{{"active":"a","keys":{{"a":"{encoded}","a":"{encoded}"}}}}"#
        ))
        .is_err());
    }
}
