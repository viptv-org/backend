use super::*;
use axum::{body::Body, http::HeaderMap};
use rand_core::{OsRng, RngCore};

const CODE_SPACE: u32 = 100_000_000;
const RANDOM_LIMIT: u32 = (u32::MAX / CODE_SPACE) * CODE_SPACE;
const MAX_CODE_ATTEMPTS: usize = 10;

fn format_random_code(value: u32) -> Option<String> {
    // Reject the incomplete final range rather than biasing codes with modulo.
    (value < RANDOM_LIMIT).then(|| format!("{:08}", value % CODE_SPACE))
}

fn random_user_code() -> Result<String, ApiError> {
    loop {
        let mut bytes = [0; 4];
        OsRng.try_fill_bytes(&mut bytes).map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not generate pairing code".into(),
            )
        })?;
        if let Some(code) = format_random_code(u32::from_le_bytes(bytes)) {
            return Ok(code);
        }
    }
}

pub(super) fn valid_user_code(code: &str) -> bool {
    (code.len() == 8 && code.bytes().all(|byte| byte.is_ascii_digit()))
        || (code.len() == 10 && code.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub(super) fn create_pairing(db: &Connection, name: &str) -> Result<(String, String), ApiError> {
    insert_pairing(db, name, random_user_code)
}

pub(super) fn insert_pairing(
    db: &Connection,
    name: &str,
    mut generate: impl FnMut() -> Result<String, ApiError>,
) -> Result<(String, String), ApiError> {
    let device_code = token();
    for _ in 0..MAX_CODE_ATTEMPTS {
        let user_code = generate()?;
        // Reserve atomically; never replace another pending or approved TV.
        let inserted = db.execute(
            "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,?3,?4) ON CONFLICT(code_hash) DO NOTHING",
            params![hash(&user_code), hash(&device_code), name, now() + 600],
        ).map_err(crate::db_error)?;
        if inserted == 1 {
            return Ok((user_code, device_code));
        }
    }
    Err(ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not reserve pairing code".into(),
    ))
}

#[derive(Deserialize)]
pub(crate) struct QrQuery {
    code: String,
}
pub(crate) async fn device_qr(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<QrQuery>,
) -> Result<Response, ApiError> {
    crate::blocking(move || {
        let code = query.code.trim().to_uppercase();
        if !valid_user_code(&code) {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "Invalid device code".into(),
            ));
        }
        let db = app.db.lock().map_err(|_| unauthorized())?;
        // The public QR endpoint must not be an unlimited code-existence oracle.
        rate(&db, "auth:global:/auth/device/qr", 600)?;
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_pairings WHERE code_hash=?1 AND expires>?2)",
                params![hash(&code), now()],
                |row| row.get(0),
            )
            .map_err(crate::db_error)?;
        if !exists {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Invalid or expired pairing code".into(),
            ));
        }
        let origin = canonical_origin()?
            .map(|value| value.to_string().trim_end_matches('/').to_owned())
            .or_else(|| {
                headers
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
                    .map(|host| format!("https://{host}"))
            })
            .ok_or_else(forbidden)?;
        let target = format!("{origin}/device?code={code}");
        let qr = qrcode::QrCode::new(target.as_bytes()).map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "QR generation failed".into(),
            )
        })?;
        let quiet = 4usize;
        let scale = 8usize;
        let modules = qr.width();
        let side = (modules + quiet * 2) * scale;
        let colors = qr.to_colors();
        let mut pixels = vec![255u8; side * side];
        for y in 0..modules {
            for x in 0..modules {
                if colors[y * modules + x] == qrcode::Color::Dark {
                    let start_y = (y + quiet) * scale;
                    let start_x = (x + quiet) * scale;
                    for py in start_y..start_y + scale {
                        pixels[py * side + start_x..py * side + start_x + scale].fill(0);
                    }
                }
            }
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, side as u32, side as u32);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR generation failed".into(),
                )
            })?;
            writer.write_image_data(&pixels).map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR generation failed".into(),
                )
            })?;
        }
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(png))
            .map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR response failed".into(),
                )
            })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_codes_preserve_zeroes_and_reject_biased_random_tail() {
        assert_eq!(format_random_code(0).as_deref(), Some("00000000"));
        assert_eq!(format_random_code(123456).as_deref(), Some("00123456"));
        assert_eq!(
            format_random_code(RANDOM_LIMIT - 1).as_deref(),
            Some("99999999")
        );
        assert_eq!(format_random_code(RANDOM_LIMIT), None);
        assert_eq!(format_random_code(u32::MAX), None);
    }
}
