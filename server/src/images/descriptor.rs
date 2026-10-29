use url::Url;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct Image {
    pub folder: String,
    pub asset: String,
}
impl Image {
    pub fn parts(folder: &str, asset: &str) -> Option<Self> {
        if asset.len() > 160 || !matches!(folder, "posters" | "fanart" | "episodes") {
            return None;
        }
        let (stem, extension) = asset.rsplit_once('.')?;
        if !matches!(extension, "webp" | "jpg" | "png" | "gif") {
            return None;
        }
        let (path, size) = stem.rsplit_once('_')?;
        if path.is_empty()
            || path.len() > 128
            || path
                .split('/')
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return None;
        }
        let valid = match folder {
            "posters" => matches!(size, "s" | "cm" | "c" | "ca" | "m" | "w"),
            "episodes" => matches!(size, "m" | "c" | "w"),
            "fanart" => matches!(size, "w" | "medium" | "mobile" | "s48" | "d"),
            _ => false,
        };
        if !valid || (size == "d" && extension != "jpg") {
            return None;
        }
        let size = if folder == "fanart" && size == "w" {
            "medium"
        } else {
            size
        };
        Some(Self {
            folder: folder.into(),
            asset: format!("{path}_{size}.{extension}"),
        })
    }
    pub fn href(href: &str) -> Option<Self> {
        let source = if href.starts_with("/api/images/simkl/") {
            href.strip_prefix("/api/images/simkl/")?.to_owned()
        } else {
            let mut url = Url::parse(href).ok()?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            if url.host_str() == Some("wsrv.nl") {
                let target = url.query_pairs().find(|(k, _)| k == "url")?.1.into_owned();
                url = Url::parse(&target).ok()?;
            }
            if url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none()
            {
                if let Some(path) = url.path().strip_prefix("/api/images/simkl/") {
                    let (folder, asset) = path.split_once('/')?;
                    return Self::parts(folder, asset);
                }
            }
            if url.scheme() != "https"
                || url.host_str() != Some("simkl.in")
                || url.port().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return None;
            }
            url.path().strip_prefix('/')?.into()
        };
        let (folder, asset) = source.split_once('/')?;
        Self::parts(folder, asset)
    }
    pub fn direct(&self) -> String {
        format!("https://simkl.in/{}/{}", self.folder, self.asset)
    }
    pub fn proxy(&self) -> String {
        format!("/api/images/simkl/{}/{}", self.folder, self.asset)
    }
}
