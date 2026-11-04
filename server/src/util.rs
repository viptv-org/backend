pub use viptv_provider::validate_url;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_protocols_and_userinfo() {
        for u in [
            "file:///etc/passwd",
            "concat:http://a|http://b",
            "http://user:secret@host/a",
            "ftp://host/a",
        ] {
            assert!(validate_url(u).is_err());
        }
        assert!(validate_url("http://192.168.1.2:8080/a").is_ok());
    }
}

pub fn now_millis()->i64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().min(i64::MAX as u128) as i64}
