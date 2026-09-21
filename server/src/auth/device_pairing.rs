use super::*;
use axum::{body::Body, http::HeaderMap};

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
        if code.len() != 10 || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "Invalid device code".into(),
            ));
        }
        let db = app.db.lock().map_err(|_| unauthorized())?;
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
