use super::types::*;
use crux_core::{Command, render::render};
use crux_http::protocol::{HttpHeader, HttpRequest};
use serde_json::json;

pub(super) fn storage(
    model: &Model,
    operation: StorageOperation,
    purpose: StoragePurpose,
) -> Command<Effect, Event> {
    let epoch = model.epoch;
    Command::request_from_shell(operation)
        .then_send(move |result| Event::StorageCompleted(epoch, purpose, result))
}
pub(super) fn request(
    model: &Model,
    path: &str,
    method: &str,
    body: Option<serde_json::Value>,
    purpose: HttpPurpose,
) -> Command<Effect, Event> {
    let epoch = model.epoch;
    let mut headers = vec![HttpHeader {
        name: "Accept".into(),
        value: "application/json".into(),
    }];
    if let Some(tokens) = &model.tokens
        && !matches!(purpose, HttpPurpose::Refresh)
    {
        headers.push(HttpHeader {
            name: "Authorization".into(),
            value: format!("Bearer {}", tokens.access_token),
        });
    }
    if body.is_some() {
        headers.push(HttpHeader {
            name: "Content-Type".into(),
            value: "application/json".into(),
        });
    }
    let operation = HttpRequest {
        method: method.into(),
        url: format!("{}{path}", model.origin),
        headers,
        body: body.map(|b| b.to_string().into_bytes()).unwrap_or_default(),
    };
    Command::request_from_shell(operation)
        .then_send(move |result| Event::HttpCompleted(epoch, purpose, result))
}
pub(super) fn me(model: &Model, refreshed: bool) -> Command<Effect, Event> {
    request(
        model,
        "/api/auth/me",
        "GET",
        None,
        HttpPurpose::Identity { refreshed },
    )
}
pub(super) fn fail(model: &mut Model, message: &str) -> Command<Effect, Event> {
    model.view.phase = Phase::Error;
    model.view.error = Some(message.into());
    model.view.error_status = None;
    render()
}
pub(super) fn fail_response(
    model: &mut Model,
    status: u16,
    message: &str,
) -> Command<Effect, Event> {
    let command = fail(model, message);
    model.view.error_status = Some(status);
    command
}
pub(super) fn save(model: &Model, purpose: StoragePurpose) -> Command<Effect, Event> {
    storage(
        model,
        StorageOperation::Save(
            serde_json::to_string(&model.tokens).expect("session is serializable"),
        ),
        purpose,
    )
}
pub(super) fn select(model: &mut Model, id: String) -> Command<Effect, Event> {
    model.view.phase = Phase::Selecting;
    request(
        model,
        "/api/auth/profile",
        "POST",
        Some(json!({"profile_id":id})),
        HttpPurpose::Select(id),
    )
    .and(render())
}
pub(super) fn finish_identity(model: &mut Model, identity: Identity) -> Command<Effect, Event> {
    let accepted_profile = model.accepted_profile.take();
    let server_profile = identity.profile_id.clone().filter(|id| {
        identity
            .profiles
            .iter()
            .any(|p| &p.id == id && p.setup_complete != Some(false))
    });
    let remembered = model
        .tokens
        .as_ref()
        .and_then(|s| s.profile_id.clone())
        .filter(|id| {
            identity
                .profiles
                .iter()
                .any(|p| &p.id == id && p.setup_complete != Some(false))
        });
    model.view.identity = Some(identity);
    if let Some(id) = server_profile {
        model.view.selected_profile_id = Some(id.clone());
        model.view.phase = Phase::Ready;
        if let Some(s) = &mut model.tokens
            && s.profile_id.as_ref() != Some(&id)
        {
            s.profile_id = Some(id);
            // This identity already confirms the server's profile. Persist the
            // remembered choice without requesting that same identity again.
            return save(model, StoragePurpose::ConfirmedProfile);
        }
        render()
    } else if accepted_profile.is_some() {
        model.view.selected_profile_id = None;
        if remembered.is_some() {
            fail(
                model,
                "The server did not confirm the selected profile. Please retry.",
            )
        } else {
            model.view.phase = Phase::Profiles;
            render()
        }
    } else if let Some(id) = remembered {
        select(model, id)
    } else {
        model.view.selected_profile_id = None;
        model.view.phase = Phase::Profiles;
        render()
    }
}
