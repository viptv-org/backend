//! Public Simkl pixels only: no arbitrary URLs, credentials or account data.
use crate::*;
use std::time::Instant;
mod cache;
mod descriptor;
pub(crate) use cache::Service;
pub(crate) async fn get(
    State(app): State<App>,
    Path((folder, asset)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let image = descriptor::Image::parts(&folder, &asset)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Invalid image reference".into()))?;
    let asset = app.images.load(image).await?;
    Ok((
        [
            (header::CONTENT_TYPE, asset.content_type),
            (header::CACHE_CONTROL, "public, max-age=86400, immutable"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        axum::body::Bytes::copy_from_slice(&asset.bytes),
    )
        .into_response())
}
pub(crate) async fn options() -> Response {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS"),
            (
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "accept, range, content-type, x-viptv-image-delivery",
            ),
            (header::ACCESS_CONTROL_MAX_AGE, "86400"),
        ],
    )
        .into_response()
}
pub(crate) async fn negotiate(request: Request, next: Next) -> Response {
    let mode = request
        .headers()
        .get("x-viptv-image-delivery")
        .and_then(|v| v.to_str().ok())
        .filter(|v| matches!(*v, "direct" | "proxy"))
        .map(str::to_owned);
    let response = next.run(request).await;
    let Some(mode) = mode else {
        return response;
    };
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return ApiError(
                StatusCode::BAD_GATEWAY,
                "Image representation unavailable".into(),
            )
            .into_response();
        }
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Response::from_parts(parts, axum::body::Body::from(bytes)),
    };
    rewrite(&mut value, &mode);
    let bytes = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes,
        Err(_) => {
            return ApiError(
                StatusCode::BAD_GATEWAY,
                "Image representation unavailable".into(),
            )
            .into_response();
        }
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert(header::VARY, "x-viptv-image-delivery".parse().unwrap());
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
fn rewrite(value: &mut Value, mode: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite(value, mode);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "poster" | "landscape" | "logo" | "background" | "thumbnail" | "icon"
                ) {
                    if let Some(image) = value.as_str().and_then(descriptor::Image::href) {
                        *value = Value::String(if mode == "direct" {
                            image.direct()
                        } else {
                            image.proxy()
                        });
                    }
                } else {
                    rewrite(value, mode);
                }
            }
        }
        _ => {}
    }
}
#[cfg(test)]
mod tests;
