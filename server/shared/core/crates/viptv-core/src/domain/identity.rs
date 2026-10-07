use super::*;
use std::borrow::Cow;

fn transport_key(key: &str) -> bool {
    // Most provider keys are already lowercase ASCII; leave those borrowed.
    let key = if key
        .bytes()
        .all(|byte| byte.is_ascii() && !byte.is_ascii_uppercase() && byte != b'_' && byte != b'-')
    {
        Cow::Borrowed(key)
    } else {
        Cow::Owned(key.to_lowercase().replace(['_', '-'], ""))
    };
    // One pass avoids fifteen general substring searches per metadata key.
    let bytes = key.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        let tail = &bytes[index..];
        match byte {
            b'a' => tail.starts_with(b"authorization") || tail.starts_with(b"accesstoken"),
            b'c' => tail.starts_with(b"cookie") || tail.starts_with(b"credential"),
            b'd' => tail.starts_with(b"devicecode") || tail.starts_with(b"devicetoken"),
            b'h' => tail.starts_with(b"header"),
            b'l' => tail.starts_with(b"link"),
            b'o' => tail.starts_with(b"origin"),
            b'p' => tail.starts_with(b"password") || tail.starts_with(b"proxy"),
            b'r' => tail.starts_with(b"refreshtoken") || tail.starts_with(b"referer"),
            b'u' => tail.starts_with(b"url") || tail.starts_with(b"uri"),
            _ => false,
        }
    })
}

pub fn clean(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.iter().map(clean).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| !transport_key(key))
                .map(|(k, v)| (k.clone(), clean(v)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

pub(super) fn clean_in_place(v: &mut Value) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(clean_in_place),
        Value::Object(fields) => fields.retain(|key, value| {
            if transport_key(key) {
                false
            } else {
                clean_in_place(value);
                true
            }
        }),
        _ => {}
    }
}

pub fn profile(v: &Value) -> Result<Value> {
    obj(v)?;
    let mut out = json!({"id":id(v,"id")?,"name":string(v,"name")?,"raw":clean(v)});
    for (a, b) in [
        ("primary", "primary"),
        ("is_primary", "primary"),
        ("avatar_style", "avatarStyle"),
        ("avatar_choice", "avatarChoice"),
    ] {
        copy(v, &mut out, a, b);
    }
    if let Some(avatar) = v["avatar"].as_str().or_else(|| v["avatar_url"].as_str()) {
        out["avatar"] = json!(avatar);
    }
    if let Some(kid) = v["kids"]
        .as_bool()
        .or_else(|| v["kid"].as_bool())
        .or_else(|| v["is_kids"].as_bool())
    {
        out["kid"] = json!(kid);
    }
    if let Some(complete) = v["setup_complete"].as_bool() {
        out["setupComplete"] = json!(complete);
    }
    Ok(out)
}
pub fn identity(v: &Value) -> Result<Value> {
    let a = &v["account"];
    Ok(
        json!({"account":{"id":id(a,"id")?,"username":string(a,"username")?,"name":string(a,"name")?,"role":string(a,"role")?},
        "profiles":array(v,"profiles")?.iter().map(profile).collect::<Result<Vec<_>>>()?,
        "profileId":id(v,"profile_id").ok(),"restricted":boolean(v,"restricted")?,"profileSetupRequired":boolean(v,"profile_setup_required")?}),
    )
}
pub fn tokens(v: &Value) -> Result<Value> {
    Ok(
        json!({"sessionId":id(v,"session_id")?,"accountId":id(v,"account_id")?,"profileId":id(v,"profile_id").ok(),"accessToken":string(v,"access_token")?,"refreshToken":string(v,"refresh_token")?,"expiresIn":number(v,"expires_in")?}),
    )
}

#[cfg(test)]
mod tests {
    use super::transport_key;

    #[test]
    fn transport_key_matches_original_redaction_for_case_separators_and_unicode() {
        let forbidden = [
            "url",
            "uri",
            "link",
            "header",
            "authorization",
            "accesstoken",
            "refreshtoken",
            "devicecode",
            "devicetoken",
            "cookie",
            "password",
            "credential",
            "proxy",
            "referer",
            "origin",
        ];
        for word in
            forbidden
                .into_iter()
                .chain(["id", "name", "type", "addon_id", "映画", "İ", "Café 🎬"])
        {
            for spelling in [
                word.to_owned(),
                word.to_uppercase(),
                word.chars().map(|c| format!("{c}_-")).collect::<String>(),
            ] {
                for prefix in ["", "a", "u", "d", "映画"] {
                    let key = format!("{prefix}{spelling}SUFFIX");
                    let lowered = key.to_lowercase().replace(['_', '-'], "");
                    assert_eq!(
                        transport_key(&key),
                        forbidden.iter().any(|word| lowered.contains(word)),
                        "{key}"
                    );
                }
            }
        }
    }
}
