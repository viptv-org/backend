//! Optional artwork uses only the authoritative metadata's explicit IMDb mapping.
use super::*;
use viptv_engine::presentation::Card;

pub(crate) async fn enrich(app: &App, card: &mut Card) {
    enrich_service(&app.simkl, card).await;
}
pub(super) async fn enrich_service(service: &Service, card: &mut Card) {
    if card.logo.is_some() || !service.public_artwork() {
        return;
    }
    let Some(imdb) = card
        .identity
        .ids
        .iter()
        .find(|id| id.namespace == "imdb")
        .map(|id| id.value.as_str())
    else {
        return;
    };
    if !imdb
        .strip_prefix("tt")
        .is_some_and(|s| !s.is_empty() && s.len() <= 14 && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return;
    }
    let kind = card.identity.kind.as_str();
    if !matches!(kind, "movie" | "series") {
        return;
    }
    let key = format!("artwork:{kind}:{imdb}");
    let entry = {
        let mut cache = service.metadata.lock().await;
        if cache.len() >= 256 && !cache.contains_key(&key) {
            cache.retain(|_, entry| Arc::strong_count(entry) > 1);
            if cache.len() >= 256 {
                return;
            }
        }
        cache.entry(key).or_default().clone()
    };
    let mut cached = entry.lock().await;
    let value = if let Some((_, value)) = cached
        .as_ref()
        .filter(|(created, _)| created.elapsed() < Duration::from_secs(3600))
    {
        value.clone()
    } else {
        // This is the canonical public logo asset path returned by Cinemeta.
        // Verify the image itself; fetching full series metadata just for artwork is unnecessary.
        let Ok(url) = url::Url::parse(&format!(
            "https://images.metahub.space/logo/medium/{imdb}/img"
        )) else {
            return;
        };
        let Ok(bytes) =
            crate::source_http::bytes(url, 512 * 1024, Duration::from_secs(2), false, true).await
        else {
            return;
        };
        let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
        let webp = bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP");
        if !png && !webp {
            return;
        }
        let value = Arc::new(
            json!({"logo":format!("https://images.metahub.space/logo/medium/{imdb}/img")}),
        );
        *cached = Some((Instant::now(), value.clone()));
        value
    };
    card.logo = verified_logo(value["logo"].as_str());
}
fn verified_logo(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    if raw.len() > 4096 {
        return None;
    };
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(
            url.host_str(),
            Some("images.metahub.space" | "assets.fanart.tv" | "image.tmdb.org")
        )
    {
        return None;
    }
    Some(raw.into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artwork_never_accepts_credentials_private_hosts_or_plain_http() {
        assert!(
            verified_logo(Some(
                "https://images.metahub.space/logo/medium/tt0409591/img"
            ))
            .is_some()
        );
        for raw in [
            "http://images.metahub.space/logo.png",
            "https://127.0.0.1/logo",
            "https://images.metahub.space/a?token=secret",
            "https://secret@images.metahub.space/a",
        ] {
            assert!(verified_logo(Some(raw)).is_none());
        }
    }
}

#[cfg(test)]
mod qualification {
    use super::*;
    #[tokio::test]
    #[ignore = "Owned public artwork egress qualification"]
    async fn public_logo_asset_reader_returns_real_image_without_metadata_payload() {
        let service = Service::fixture("https://api.simkl.com");
        let mut card = viptv_engine::simkl::card(
            &json!({"title":"Public movie","ids":{"simkl":1902343,"imdb":"tt22084616"}}),
            "movie",
        )
        .unwrap();
        enrich_service(&service, &mut card).await;
        assert!(card.logo.is_some(), "verified PNG must project a logo");
        let url =
            url::Url::parse("https://images.metahub.space/logo/medium/tt22084616/img").unwrap();
        match crate::source_http::bytes(url, 512 * 1024, Duration::from_secs(2), false, true).await
        {
            Ok(bytes) => {
                println!(
                    "Artwork bytes={} png={} webp={}",
                    bytes.len(),
                    bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                    bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
                );
                assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"RIFF"));
            }
            Err(error) => panic!("Artwork egress failure: {}", error.provider_code()),
        }
    }
}
