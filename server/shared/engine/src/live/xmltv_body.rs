use crate::Error;
/// XMLTV commonly ships as .xml.gz without a Content-Encoding header.
pub fn decode_xmltv_body(body: &[u8]) -> Result<String, Error> {
    decode(body, 16 * 1024 * 1024, Error::InvalidQuery)
}
/// A playlist document (M3U or Xtream JSON): the same gzip-aware decode under the
/// shared playlist bound, where overflow is a distinct error instead of a truncation.
pub fn decode_body(body: &[u8], limit: usize) -> Result<String, Error> {
    decode(body, limit, Error::TooLarge)
}
fn decode(body: &[u8], limit: usize, overflow: Error) -> Result<String, Error> {
    use std::io::Read;
    if body.len() > limit {
        return Err(overflow);
    }
    let decoded = if body.starts_with(&[0x1f, 0x8b]) {
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(body)
            .take((limit + 1) as u64)
            .read_to_end(&mut decoded)
            .map_err(|_| Error::InvalidQuery)?;
        if decoded.len() > limit {
            return Err(overflow);
        }
        decoded
    } else {
        body.to_vec()
    };
    String::from_utf8(decoded).map_err(|_| Error::InvalidQuery)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gzip_xmltv_is_bounded_and_invalid_streams_are_rejected() {
        use std::io::{Read, Write};
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(b"<tv><channel id=\"fixture\"/></tv>")
            .unwrap();
        let gzip = encoder.finish().unwrap();
        assert_eq!(
            decode_xmltv_body(&gzip).unwrap(),
            "<tv><channel id=\"fixture\"/></tv>"
        );
        assert!(decode_xmltv_body(&gzip[..gzip.len() - 2]).is_err());
        assert!(decode_xmltv_body(&[0xff, 0xfe]).is_err());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::copy(
            &mut std::io::repeat(b' ').take(16 * 1024 * 1024 + 1),
            &mut encoder,
        )
        .unwrap();
        assert!(decode_xmltv_body(&encoder.finish().unwrap()).is_err());
        assert_eq!(
            decode_body(&[0x1f, 0x8b, b'a'], 2),
            Err(Error::TooLarge)
        );
    }
}
