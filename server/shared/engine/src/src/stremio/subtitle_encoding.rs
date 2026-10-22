use crate::Error;
/// Additive VTT delivery; the original opaque native URI remains available for
/// ASS, archives and other formats outside the shared SRT/VTT projection.
pub fn subtitle_bytes_vtt(bytes: &[u8], content_type: Option<&str>) -> Result<String, Error> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let charset = content_type.and_then(|value| {
        value.split(';').skip(1).find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k.trim().eq_ignore_ascii_case("charset")).then(|| v.trim().trim_matches('"'))
        })
    });
    let (encoding, payload) = if let Some((encoding, len)) = encoding_rs::Encoding::for_bom(bytes) {
        (encoding, &bytes[len..])
    } else if let Some(charset) = charset {
        (
            encoding_rs::Encoding::for_label_no_replacement(charset.as_bytes())
                .ok_or(Error::InvalidQuery)?,
            bytes,
        )
    } else if std::str::from_utf8(bytes).is_ok() {
        (encoding_rs::UTF_8, bytes)
    } else {
        (encoding_rs::WINDOWS_1252, bytes)
    };
    let text = encoding
        .decode_without_bom_handling_and_without_replacement(payload)
        .ok_or(Error::InvalidQuery)?;
    if text.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    super::subtitle_text::subtitle_vtt(&text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_accents_and_bom_are_preserved_without_lossy_replacements() {
        let raw = b"1\r\n00:00:01,000 --> 00:00:02,000\r\nCaf\xe9 \x97 oui\r\n";
        for hint in [None, Some("application/x-subrip; charset=Windows-1252")] {
            let vtt = subtitle_bytes_vtt(raw, hint).unwrap();
            assert!(vtt.contains("Café — oui"));
            assert!(!vtt.contains('\u{fffd}'));
        }
        assert!(subtitle_bytes_vtt(raw, Some("text/plain; charset=utf-8")).is_err());
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n日本語\n";
        let utf16 = std::iter::once(0xfeffu16)
            .chain(srt.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!(
            subtitle_bytes_vtt(&utf16, Some("text/plain; charset=windows-1252"))
                .unwrap()
                .contains("日本語")
        );
        assert!(subtitle_bytes_vtt(b"[Script Info]\nTitle: ASS", None).is_err());
        assert!(subtitle_bytes_vtt(b"PK\x03\x04archive", None).is_err());
    }
}
