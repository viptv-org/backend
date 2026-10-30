use super::*;

const BUDGET: Duration = Duration::from_secs(8);

pub(crate) async fn segments(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    let card = entry
        .proof
        .metadata_proof
        .as_ref()
        .map(|proof| proof.card.clone());
    let value = tokio::time::timeout(
        BUDGET,
        crate::engine_v3::segments::answer(&app, &lease, card.as_ref()),
    )
    .await
    .unwrap_or_else(|_| crate::engine_v3::segments::empty());
    Ok(Json(value))
}
