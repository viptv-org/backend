use serde_json::Value;
use std::borrow::Cow;

pub(crate) fn metadata_evidence<'a>(producer: &str, card: &'a Value) -> Cow<'a, Value> {
    if !producer.starts_with("vsources:") {
        return Cow::Borrowed(card);
    }
    // SDK display text must never refill facts left unknown by its typed projection.
    let fields = [
        "presentation", "quality_height", "audio_languages", "reported_languages",
        "language", "languages", "size_bytes", "release_type", "three_d",
        "dynamic_range", "dv_base_layer", "video_codec", "audio_flags", "seeders",
        "subbed", "subtitle_languages",
    ].into_iter().filter_map(|key| card.get(key).map(|value| (key.to_owned(), value.clone()))).collect();
    Cow::Owned(Value::Object(fields))
}
