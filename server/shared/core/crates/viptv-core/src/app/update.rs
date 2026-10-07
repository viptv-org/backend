use super::commands::{fail, fail_response, finish_identity, me, request, save, select, storage};
use super::types::*;
use crate::domain;
use crux_core::{App, Command, render::render};
use crux_http::protocol::HttpResult;
use serde_json::json;

impl App for Viptv {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewModel;
    type Effect = Effect;
    fn view(&self, model: &Model) -> ViewModel {
        model.view.clone()
    }
    fn update(&self, event: Event, model: &mut Model) -> Command<Effect, Event> {
        match event {
            Event::Begin {
                origin,
                allow_insecure_preview,
            } => {
                let Ok(url) = url::Url::parse(&origin) else {
                    return fail(model, "Invalid server origin");
                };
                if (url.scheme() != "https" && !(allow_insecure_preview && url.scheme() == "http"))
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return fail(model, "Invalid server origin");
                }
                model.epoch += 1;
                model.accepted_profile = None;
                model.refresh_attempted = false;
                model.origin = url.origin().ascii_serialization();
                model.view = ViewModel {
                    phase: Phase::Restoring,
                    ..Default::default()
                };
                storage(model, StorageOperation::Load, StoragePurpose::Load).and(render())
            }
            Event::Retry => {
                model.epoch += 1;
                model.accepted_profile = None;
                model.refresh_attempted = false;
                model.view.error = None;
                model.view.error_status = None;
                if model.tokens.is_some() {
                    model.view.phase = Phase::Checking;
                    me(model, false).and(render())
                } else {
                    model.view.phase = Phase::Restoring;
                    storage(model, StorageOperation::Load, StoragePurpose::Load).and(render())
                }
            }
            Event::AdoptSession { tokens_json } => {
                let Ok(tokens) = serde_json::from_str::<Session>(&tokens_json) else {
                    return fail(model, "Invalid saved session");
                };
                if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
                    return fail(model, "Invalid saved session");
                }
                model.epoch += 1;
                model.accepted_profile = None;
                model.refresh_attempted = false;
                model.tokens = Some(tokens);
                model.view = ViewModel {
                    phase: Phase::Checking,
                    ..Default::default()
                };
                save(model, StoragePurpose::Adopt).and(render())
            }
            Event::SelectProfile { profile_id } => {
                if !matches!(
                    model.view.phase,
                    Phase::Profiles | Phase::Ready | Phase::Error
                ) || !model
                    .view
                    .identity
                    .as_ref()
                    .is_some_and(|i| i.profiles.iter().any(|p| p.id == profile_id))
                {
                    return Command::done();
                }
                model.epoch += 1;
                model.accepted_profile = None;
                model.refresh_attempted = false;
                model.view.error = None;
                model.view.error_status = None;
                select(model, profile_id)
            }
            Event::SignOut => {
                model.view.error = None;
                model.view.error_status = None;
                if model.tokens.is_none() {
                    return Command::done();
                }
                model.epoch += 1;
                model.accepted_profile = None;
                model.refresh_attempted = false;
                request(
                    model,
                    "/api/auth/logout",
                    "POST",
                    Some(json!({})),
                    HttpPurpose::Logout,
                )
            }
            Event::StorageCompleted(epoch, purpose, result) => {
                if epoch != model.epoch {
                    return Command::done();
                }
                let StorageResult::Ok(value) = result else {
                    return fail(
                        model,
                        "Could not access saved session. Retry to stay paired.",
                    );
                };
                match purpose {
                    StoragePurpose::Load => match value {
                        None => {
                            model.tokens = None;
                            model.view.phase = Phase::Pairing;
                            render()
                        }
                        Some(value) => {
                            let Ok(tokens) = serde_json::from_str::<Session>(&value) else {
                                return fail(model, "Saved session could not be read");
                            };
                            model.tokens = Some(tokens);
                            model.view.phase = Phase::Checking;
                            me(model, false).and(render())
                        }
                    },
                    StoragePurpose::Refresh => {
                        model.view.phase = Phase::Checking;
                        match model.refresh_next.take() {
                            Some(HttpPurpose::Select(id)) => select(model, id),
                            Some(HttpPurpose::Logout) => request(
                                model,
                                "/api/auth/logout",
                                "POST",
                                Some(json!({})),
                                HttpPurpose::Logout,
                            ),
                            _ => me(model, true),
                        }
                    }
                    StoragePurpose::Adopt => me(model, false),
                    StoragePurpose::Profile => me(model, true),
                    StoragePurpose::Clear => {
                        model.tokens = None;
                        model.view = ViewModel {
                            phase: Phase::Pairing,
                            ..Default::default()
                        };
                        render()
                    }
                }
            }
            Event::HttpCompleted(epoch, purpose, result) => {
                if epoch != model.epoch {
                    return Command::done();
                }
                let HttpResult::Ok(response) = result else {
                    return fail(model, "Unable to connect. Your saved session is retained.");
                };
                if response.status == 401 {
                    if matches!(
                        purpose,
                        HttpPurpose::Refresh | HttpPurpose::Identity { refreshed: true }
                    ) || model.refresh_attempted
                    {
                        return storage(model, StorageOperation::Clear, StoragePurpose::Clear);
                    }
                    let Some(tokens) = &model.tokens else {
                        model.view.phase = Phase::Pairing;
                        return render();
                    };
                    model.refresh_attempted = true;
                    model.refresh_next = Some(purpose);
                    return request(
                        model,
                        "/api/auth/device/refresh",
                        "POST",
                        Some(json!({"refresh_token":tokens.refresh_token})),
                        HttpPurpose::Refresh,
                    );
                }
                if !(200..300).contains(&response.status) {
                    let mut error = if response.body.len() <= 4096 {
                        serde_json::from_slice::<serde_json::Value>(&response.body)
                            .ok()
                            .filter(|v| v.is_object())
                            .unwrap_or(json!({}))
                    } else {
                        json!({})
                    };
                    error["status"] = json!(response.status);
                    let display = domain::api_error(&error);
                    let command = fail(
                        model,
                        display["message"].as_str().unwrap_or("Request failed"),
                    );
                    model.view.error_status = Some(response.status);
                    return command;
                }
                if response.body.len() > 2 * 1024 * 1024 {
                    return fail_response(model, response.status, "Invalid server response");
                }
                let payload = if response.body.is_empty() {
                    json!({})
                } else {
                    match serde_json::from_slice(&response.body) {
                        Ok(v) => v,
                        Err(_) => {
                            return fail_response(
                                model,
                                response.status,
                                "Invalid server response",
                            );
                        }
                    }
                };
                match purpose {
                    HttpPurpose::Identity { .. } => {
                        let identity = domain::identity(&payload).and_then(|v| {
                            serde_json::from_value(v).map_err(|_| crate::CoreError::InvalidInput)
                        });
                        match identity {
                            Ok(identity) => finish_identity(model, identity),
                            Err(_) => {
                                fail_response(model, response.status, "Invalid server response")
                            }
                        }
                    }
                    HttpPurpose::Refresh => {
                        let session = domain::tokens(&payload).and_then(|v| {
                            serde_json::from_value::<Session>(v)
                                .map_err(|_| crate::CoreError::InvalidInput)
                        });
                        match session {
                            Ok(mut session) => {
                                if session.profile_id.is_none()
                                    && model
                                        .tokens
                                        .as_ref()
                                        .is_some_and(|old| old.account_id == session.account_id)
                                {
                                    session.profile_id =
                                        model.tokens.as_ref().and_then(|s| s.profile_id.clone());
                                }
                                model.tokens = Some(session);
                                save(model, StoragePurpose::Refresh)
                            }
                            Err(_) => {
                                fail_response(model, response.status, "Invalid server response")
                            }
                        }
                    }
                    HttpPurpose::Select(id) => {
                        model.accepted_profile = Some(id.clone());
                        if let Some(tokens) = &mut model.tokens {
                            tokens.profile_id = Some(id);
                        }
                        save(model, StoragePurpose::Profile)
                    }
                    HttpPurpose::Logout => {
                        storage(model, StorageOperation::Clear, StoragePurpose::Clear)
                    }
                }
            }
        }
    }
}
