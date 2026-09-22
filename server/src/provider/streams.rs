use super::*;

impl ProviderService {
    /// Compatibility collector; production discovery publishes each bounded batch immediately.
    pub async fn streams(&self, request: Value) -> Result<Vec<Value>, String> {
        let mut streams = Vec::new();
        let mut error = None;
        self.stream_batches(request, |_, result| match result {
            Ok(batch) => streams.extend(batch),
            Err(e) => error = Some(e),
        })
        .await?;
        if streams.is_empty() {
            if let Some(error) = error {
                return Err(error);
            }
        }
        let mut seen = HashSet::new();
        streams.retain(|s| seen.insert(s["url"].as_str().unwrap_or_default().to_owned()));
        Ok(streams)
    }

    /// At most 100 candidates, eight concurrent candidate lookups, four network requests.
    /// Each timeout covers its own candidate including queued permits, never earlier results.
    pub async fn stream_batches(
        &self,
        request: Value,
        mut publish: impl FnMut(String, Result<Vec<Value>, String>) + Send,
    ) -> Result<(), String> {
        let kind = request
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Missing stream type")?;
        if kind == "live" {
            let id = required_string(&request, "id", 256)?;
            let (provider, stream) = self.blocking(move |s| s.channel(&id)).await?;
            publish(
                format!("iptv:{}", provider.id),
                Ok(vec![
                    json!({"url":media_url(&provider.url,&provider.username,&provider.password,"live",&stream,"ts")?,"name":provider.name,"source":format!("iptv:{}",provider.id)}),
                ]),
            );
            return Ok(());
        }
        if kind != "movie" && kind != "series" {
            return Ok(());
        }
        let only_provider = request
            .get("only_provider_id")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|id| *id > 0)
                    .ok_or("Invalid provider scope")
            })
            .transpose()?;
        let parsed = MatchRequest::parse(&request, kind)?;
        if kind == "series" && (parsed.season.is_none() || parsed.episode.is_none()) {
            return Err("Series streams require season and episode".into());
        }
        let season = parsed.season.unwrap_or(0);
        let episode = parsed.episode.unwrap_or(0);
        let kind = kind.to_owned();
        let matching = parsed.clone();
        let chosen = self
            .blocking(move |s| {
                let candidates: Vec<_> = s
                    .candidates_filtered(Some(&kind), Some(&parsed))?
                    .into_iter()
                    .filter(|c| only_provider.is_none_or(|id| c.provider_id == id))
                    .collect();
                // Round robin providers so duplicates from one cannot occupy every work slot.
                let mut groups =
                    std::collections::BTreeMap::<i64, std::collections::VecDeque<Candidate>>::new();
                for c in select_candidates(&candidates, &parsed) {
                    groups
                        .entry(c.provider_id)
                        .or_default()
                        .push_back(c.clone());
                }
                let mut chosen = Vec::new();
                while chosen.len() < 100 {
                    let before = chosen.len();
                    for group in groups.values_mut() {
                        if chosen.len() == 100 {
                            break;
                        }
                        if let Some(c) = group.pop_front() {
                            chosen.push(c);
                        }
                    }
                    if chosen.len() == before {
                        break;
                    }
                }
                let selected: HashSet<_> = chosen.iter().map(|c| c.id.clone()).collect();
                let sparse = s.sparse_candidates(&kind, &parsed, only_provider)?;
                let mut work: Vec<_> = chosen.into_iter().map(|c| (c, false)).collect();
                let remaining = 100 - work.len();
                work.extend(
                    sparse
                        .into_iter()
                        .filter(|c| {
                            only_provider.is_none_or(|id| c.provider_id == id)
                                && !selected.contains(&c.id)
                        })
                        .take(remaining)
                        .map(|c| (c, true)),
                );
                Ok(work)
            })
            .await?;
        let mut results = stream::iter(chosen.into_iter().map(|(c, lazy)| {
            let service = self.clone();
            let matching = &matching;
            async move {
                let source = format!("iptv:{}", c.provider_id);
                let result = tokio::time::timeout(Duration::from_secs(30), async move {
                    let c = if lazy {
                        let Some(c) = service.enrich_candidate(c).await? else {
                            return Ok(Vec::new());
                        };
                        if evidence_conflicts(&c, matching)
                            || select_candidates(std::slice::from_ref(&c), matching).is_empty()
                        {
                            return Ok(Vec::new());
                        }
                        c
                    } else {
                        c
                    };
                    service.resolve_candidate(c, season, episode).await
                })
                .await
                .unwrap_or_else(|_| Err("IPTV candidate timed out".into()));
                (source, result)
            }
        }))
        .buffer_unordered(8);
        while let Some((source, result)) = results.next().await {
            publish(source, result);
        }
        Ok(())
    }
}
