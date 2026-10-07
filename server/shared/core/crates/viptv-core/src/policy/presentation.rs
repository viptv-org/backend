use super::*;

pub(super) fn presentation(v: &Value) -> Result {
    Ok({
        let m = if v["item"].is_object() { &v["item"] } else { v };
        let episode = num(m, "episode");
        let season = num(m, "season");
        let label = if episode > 0.0 {
            format!(
                "S{season:.0} E{episode:.0}{}",
                if text(m, "episodeTitle").is_empty() {
                    String::new()
                } else {
                    format!(" · {}", text(m, "episodeTitle"))
                }
            )
        } else {
            String::new()
        };
        let live = text(m, "type") == "live";
        let next = text(m, "queueStatus") == "next";
        let resume = resume_eligible(m);
        let action = if live {
            "play"
        } else if next && m["resumeActive"] != true {
            "next"
        } else if resume {
            "resume"
        } else if text(m, "type") == "series" && episode <= 0.0 {
            "episodes"
        } else {
            "sources"
        };
        json!({"heroImage":image(m,"background"),"posterImage":image(m,"poster"),"episodeImage":image(m,"thumbnail"),"titleLogo":image(m,"titleLogo"),"title":m["name"],"episodeLabel":label,"progress":if num(m,"duration")>0.0{(num(m,"position")/num(m,"duration")).clamp(0.0,1.0)}else{0.0},"primaryAction":action,"primaryActionLabel":match action{"next"=>"Play next episode","resume"=>"Resume","play"=>"Watch live","episodes"=>"Episodes",_=>"Play"},"resumeEligible":resume,"canAutoNext":!live&&episode>0.0&&num(m,"duration")>0.0&&num(m,"duration")-num(m,"position")<=10.0&&num(m,"position")>0.0})
    })
}

pub(super) fn card_presentation(v: &Value) -> Result {
    Ok({
        let m = &v["item"];
        let queue = text(v, "context") == "queue";
        let live = text(m, "type") == "live";
        let episode = num(m, "episode") > 0.0 || text(m, "type") == "episode";
        let presentation = normalize("presentation", m)?;
        let candidates: &[(&str, &str)] = if live {
            &[("poster", "logo")]
        } else if queue && episode {
            // A series poster is not an episode still. An unavailable still
            // can use an explicitly known landscape, never a portrait crop.
            &[("thumbnail", "episode"), ("background", "landscape")]
        } else {
            &[
                ("background", "landscape"),
                ("thumbnail", "landscape"),
                ("poster", "poster"),
            ]
        };
        let (art, role) = candidates
            .iter()
            .find_map(|(key, role)| {
                let art = image(m, key);
                let failed = v["failedImages"]
                    .as_array()
                    .is_some_and(|urls| urls.contains(&art));
                (!art.is_null() && !failed).then_some((art, *role))
            })
            .unwrap_or((Value::Null, "none"));
        let status = match text(m, "queueStatus") {
            "next" if m["resumeActive"] != true => "Play next episode".to_owned(),
            "caught_up" => "You're caught up".to_owned(),
            "upcoming" => "Next episode coming soon".to_owned(),
            "pending" | "unavailable" => "Find next episode".to_owned(),
            _ if !live && m["completionOnly"] != true && num(m, "position") > 0.0 => {
                let seconds = num(m, "position").floor() as u64;
                format!("Resume at {}:{:02}", seconds / 60, seconds % 60)
            }
            _ => String::new(),
        };
        let mut context = Vec::new();
        if episode {
            context.push(format!(
                "S{:.0} · E{:.0}",
                num(m, "season"),
                num(m, "episode")
            ));
            if !text(m, "episodeTitle").is_empty() {
                context.push(text(m, "episodeTitle").to_owned());
            }
        }
        if !status.is_empty() {
            context.push(status);
        }
        if context.is_empty() && !live {
            if num(m, "year") > 0.0 {
                context.push(format!("{:.0}", num(m, "year")));
            }
            if !text(m, "runtime").is_empty() {
                context.push(text(m, "runtime").to_owned());
            }
            context.extend(
                m["genres"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .take(2)
                    .map(str::to_owned),
            );
        }
        let action = if live {
            "play"
        } else if !queue {
            "details"
        } else if matches!(
            text(m, "queueStatus"),
            "caught_up" | "upcoming" | "pending" | "unavailable"
        ) {
            "episodes"
        } else {
            text(&presentation, "primaryAction")
        };
        let label = match action {
            "details" => "Details",
            "episodes" => "Episodes",
            _ => text(&presentation, "primaryActionLabel"),
        };
        json!({"image":art,"imageRole":role,"title":m["name"],"subtitle":context.join(" · "),
                "progress":if !live && num(m,"duration")>0.0 { presentation["progress"].clone() } else { Value::Null },
                "primaryAction":action,"primaryActionLabel":label})
    })
}

fn resume_eligible(m: &Value) -> bool {
    text(m, "type") != "live"
        && m["completionOnly"] != true
        && (m["resumeActive"] == true || num(m, "position") > 0.0)
}

fn progress(m: &Value) -> f64 {
    if text(m, "type") != "live" && num(m, "duration") > 0.0 {
        (num(m, "position") / num(m, "duration")).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub(super) fn home_actions(v: &Value) -> Value {
    let m = &v["item"];
    let queue = v["queueShelf"] == true;
    let previous = m["previousEpisode"].is_object();
    let manage_target = if previous { &m["previousEpisode"] } else { m };
    let live = text(m, "type") == "live";
    let resolved_next = text(m, "queueStatus") == "next" && previous && m["resumeActive"] != true;
    let manual = !queue
        && (matches!(text(m, "type"), "movie" | "episode")
            || (text(m, "type") == "series"
                && m["season"].is_number()
                && m["episode"].is_number()));
    let card_action = if queue && resolved_next {
        "next"
    } else if queue && resume_eligible(m) && num(m, "position") > 0.0 {
        "resume"
    } else {
        "details"
    };
    let hero_action = if live {
        "play"
    } else if card_action != "details" {
        card_action
    } else if manual {
        "sources"
    } else {
        "details"
    };
    let label = match hero_action {
        "play" => "Watch live",
        "next" => "Play next episode",
        "resume" => "Resume",
        "details" if text(m, "type") == "series" && !m["episode"].is_number() => "Episodes",
        _ => "Play",
    };
    json!({"canManage":!live,"managePrevious":previous,
        "canResume":resume_eligible(manage_target) && num(manage_target,"position")>0.0,"hasResolvedNext":resolved_next,
        "opensQueueManage":queue && !live,"opensSourcesFromHero":manual,
        "cardPrimaryAction":card_action,"heroPrimaryAction":hero_action,
        "heroPrimaryActionLabel":label,
        "showHeroProgress":resume_eligible(m) && num(m,"position")>0.0 && num(m,"duration")>0.0})
}

pub(super) fn episode_watching(v: &Value) -> Value {
    let m = if v["item"].is_object() { &v["item"] } else { v };
    json!({"watching":resume_eligible(m) && num(m,"position")>0.0
        && (m["resumeActive"]==true || m["watched"]!=true),"progress":progress(m)})
}

fn content_type_label(kind: &str) -> String {
    match kind {
        "movie" => "Movie".into(),
        "series" => "Series".into(),
        "anime" => "Anime".into(),
        "live" => "Live TV".into(),
        _ => kind
            .split(['.', '_'])
            .filter(|word| !word.trim().is_empty())
            .map(|word| {
                let mut chars = word.chars();
                let first = chars
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default();
                first + chars.as_str()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

pub(super) fn phone_presentation(v: &Value) -> Value {
    let shelf = &v["shelf"];
    let kind = text(shelf, "contentType");
    let heading = if shelf["isQueueShelf"] == true {
        "Continue watching".to_owned()
    } else if kind.trim().is_empty() {
        // Fixed shelves preserve server/design copy rather than title-casing it.
        text(shelf, "title").to_owned()
    } else {
        let group = catalog::type_group(kind);
        let label = if group == "other" {
            content_type_label(kind)
        } else {
            catalog::group_label(group).into()
        };
        let name = text(shelf, "catalogName");
        if name.trim().is_empty() {
            label
        } else {
            format!("{label} · {name}")
        }
    };
    let m = &v["item"];
    let context = if m["season"].is_number() && m["episode"].is_number() {
        format!("S{:.0} E{:.0}", num(m, "season"), num(m, "episode"))
    } else if let Some(year) = m["year"].as_str() {
        year.to_owned()
    } else if m["year"].is_number() {
        format!("{:.0}", num(m, "year"))
    } else {
        String::new()
    };
    json!({"shelfHeading":heading,"cardContext":context,"contentTypeLabel":content_type_label(text(v,"contentType"))})
}
