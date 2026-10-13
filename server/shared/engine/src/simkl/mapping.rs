use crate::{
    Error,
    identity::{self, MediaIdentity},
    presentation::Card,
};

/// Translate only provider-asserted IMDb identity and verified aired TV coordinates.
/// The caller keeps its canonical identity for library state and playback authority.
pub fn source_identity(parent: &Card, episode: Option<&Card>) -> Result<MediaIdentity, Error> {
    if parent.provider_id != "simkl"
        || !parent.identity.id.starts_with("simkl:")
        || parent.identity.episode.is_some()
    {
        return Err(Error::Unresolved);
    }
    let imdb = identity::resolve(&parent.identity, "imdb", &[])?;
    let mut target = parent.identity.clone();
    target.id = imdb.value.clone();
    target.ids = vec![imdb];
    match (parent.identity.kind.as_str(), episode) {
        ("movie", None) => Ok(target),
        ("series", Some(episode))
            if (parent.identity.id.starts_with("simkl:tv:")
                || parent.identity.id.starts_with("simkl:anime:"))
                && episode.provider_id == "simkl" =>
        {
            let coordinates = episode
                .identity
                .episode
                .as_ref()
                .filter(|e| e.order == "aired" && e.season.is_some() && e.episode > 0)
                .ok_or(Error::Unresolved)?;
            let season = coordinates.season.ok_or(Error::Unresolved)?;
            if episode.identity.id
                != format!("{}:{season}:{}", parent.identity.id, coordinates.episode)
            {
                return Err(Error::Unresolved);
            }
            target.id = format!("{}:{season}:{}", target.id, coordinates.episode);
            // A parent IMDb title is not an external ID for the episode itself.
            target.ids.clear();
            target.episode = Some(coordinates.clone());
            Ok(target)
        }
        _ => Err(Error::Unresolved),
    }
}
