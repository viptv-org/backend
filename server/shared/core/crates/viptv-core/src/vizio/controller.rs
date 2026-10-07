use super::failure;
use super::operations::request_for;
use super::request::{
    RequestInput, command_fields, is_setting_value, normalize_host, required_string,
    setting_fields, setting_names, validate_identity, validate_limits,
};
use super::response::{
    current_input_state, match_input, object_value, pairing_auth_token, pairing_challenge,
    parse_inputs, parse_protocol_response, parse_setting, validate_setting_snapshot,
};
use super::types::{
    VizioControllerOutput, VizioControllerOutputKind, VizioFailure, VizioFailureKind,
};
use crate::dto::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControllerConfig {
    host: String,
    auth_token: Option<String>,
    device_id: Option<String>,
    device_name: Option<String>,
    timeout_millis: Option<u64>,
    max_response_bytes: Option<u64>,
}

#[derive(Clone)]
pub(super) enum SettingMutation {
    Modify(Value),
    Action,
}

pub(super) enum PendingWorkflow {
    Direct,
    BeginPair,
    FinishPair,
    InputsCurrent {
        wanted: Option<String>,
    },
    InputsList {
        wanted: Option<String>,
        current: String,
    },
    InputFresh {
        cname: String,
    },
    InputWrite,
    SettingRead {
        category: String,
        name: String,
        mutation: SettingMutation,
        retried: bool,
    },
    SettingWrite {
        category: String,
        name: String,
        mutation: SettingMutation,
        retried: bool,
    },
}

pub struct VizioController {
    config: ControllerConfig,
    next_request_id: u32,
    pub(super) pending: Option<(u32, PendingWorkflow)>,
}
impl VizioController {
    pub fn new(config: &str) -> Result<Self, VizioFailure> {
        if config.len() > 16 * 1024 {
            return Err(failure(
                VizioFailureKind::InvalidConfig,
                "SmartCast configuration is too large",
                false,
            ));
        }
        let mut config = serde_json::from_str::<ControllerConfig>(config).map_err(|_| {
            failure(
                VizioFailureKind::InvalidConfig,
                "Invalid SmartCast configuration",
                false,
            )
        })?;
        config.host = normalize_host(&config.host)?;
        validate_identity(config.device_id.as_deref(), "device ID")?;
        validate_identity(config.device_name.as_deref(), "device name")?;
        validate_limits(config.timeout_millis, config.max_response_bytes)?;
        if config.auth_token.as_ref().is_some_and(|token| {
            token.is_empty() || token.len() > 1024 || token.chars().any(char::is_control)
        }) {
            return Err(failure(
                VizioFailureKind::InvalidConfig,
                "Invalid SmartCast credential",
                false,
            ));
        }
        Ok(Self {
            config,
            next_request_id: 1,
            pending: None,
        })
    }

    pub fn start(&mut self, operation: &str, input: &str) -> VizioControllerOutput {
        if self.pending.is_some() {
            return controller_error(failure(
                VizioFailureKind::Busy,
                "A SmartCast command is already running",
                true,
            ));
        }
        let fields = match command_fields(input) {
            Ok(fields) => fields,
            Err(error) => return controller_error(error),
        };
        match operation {
            "beginPair" => self.enqueue("beginPair", fields, PendingWorkflow::BeginPair),
            "finishPair" => self.enqueue("finishPair", fields, PendingWorkflow::FinishPair),
            "getInputs" => self.enqueue(
                "getCurrentInput",
                Map::new(),
                PendingWorkflow::InputsCurrent { wanted: None },
            ),
            "setInput" => {
                let wanted = match required_string(&fields, "input") {
                    Ok(value) if !value.trim().is_empty() && value.len() <= 128 => value.to_owned(),
                    _ => {
                        return controller_error(failure(
                            VizioFailureKind::InvalidInput,
                            "TV input is required",
                            false,
                        ));
                    }
                };
                self.enqueue(
                    "getCurrentInput",
                    Map::new(),
                    PendingWorkflow::InputsCurrent {
                        wanted: Some(wanted),
                    },
                )
            }
            "setSetting" => {
                let (category, name) = match setting_names(&fields) {
                    Ok(names) => names,
                    Err(error) => return controller_error(error),
                };
                let Some(value) = fields.get("value").cloned() else {
                    return controller_error(failure(
                        VizioFailureKind::InvalidInput,
                        "Setting value is required",
                        false,
                    ));
                };
                if !is_setting_value(&value) {
                    return controller_error(failure(
                        VizioFailureKind::InvalidParameter,
                        "Setting value must be a string, number, or boolean",
                        false,
                    ));
                }
                self.enqueue(
                    "getSetting",
                    setting_fields(&category, &name),
                    PendingWorkflow::SettingRead {
                        category,
                        name,
                        mutation: SettingMutation::Modify(value),
                        retried: false,
                    },
                )
            }
            "triggerSetting" | "blankScreen" => {
                let names = if operation == "blankScreen" {
                    Ok(("system/timers".to_owned(), "blank_screen".to_owned()))
                } else {
                    setting_names(&fields)
                };
                let (category, name) = match names {
                    Ok(names) => names,
                    Err(error) => return controller_error(error),
                };
                self.enqueue(
                    "getSetting",
                    setting_fields(&category, &name),
                    PendingWorkflow::SettingRead {
                        category,
                        name,
                        mutation: SettingMutation::Action,
                        retried: false,
                    },
                )
            }
            _ => self.enqueue(operation, fields, PendingWorkflow::Direct),
        }
    }

    pub fn resolve(&mut self, request_id: u32, status: u16, body: &str) -> VizioControllerOutput {
        let Some((expected, _)) = self.pending.as_ref() else {
            return controller_error(failure(
                VizioFailureKind::InvalidInput,
                "No SmartCast request is pending",
                false,
            ));
        };
        if *expected != request_id {
            return controller_error(failure(
                VizioFailureKind::InvalidInput,
                "Stale SmartCast response was ignored",
                false,
            ));
        }
        let (_, pending) = self.pending.take().expect("pending request checked above");
        let response = match parse_protocol_response(status, body, false) {
            Ok(response) => response,
            Err(error) => {
                if error.protocol_status.as_deref() == Some("HASHVAL_ERROR")
                    && let PendingWorkflow::SettingWrite {
                        category,
                        name,
                        mutation,
                        retried: false,
                    } = pending
                {
                    return self.enqueue(
                        "getSetting",
                        setting_fields(&category, &name),
                        PendingWorkflow::SettingRead {
                            category,
                            name,
                            mutation,
                            retried: true,
                        },
                    );
                }
                return controller_error(error);
            }
        };
        match pending {
            PendingWorkflow::Direct => controller_complete(Some(response.raw), false),
            PendingWorkflow::BeginPair => match pairing_challenge(&response) {
                Ok(challenge) => controller_complete(
                    object_value(json!({
                        "challengeType": challenge.challenge_type,
                        "token": challenge.token,
                    })),
                    false,
                ),
                Err(error) => controller_error(error),
            },
            PendingWorkflow::FinishPair => match pairing_auth_token(&response) {
                Ok(token) => {
                    self.config.auth_token = Some(token);
                    controller_complete(object_value(json!({"paired": true})), true)
                }
                Err(error) => controller_error(error),
            },
            PendingWorkflow::InputsCurrent { wanted } => match current_input_state(&response) {
                Ok((current, _)) => self.enqueue(
                    "getInputs",
                    Map::new(),
                    PendingWorkflow::InputsList { wanted, current },
                ),
                Err(error) => controller_error(error),
            },
            PendingWorkflow::InputsList { wanted, current } => {
                let inputs = parse_inputs(&response, &current);
                if let Some(wanted) = wanted {
                    let target = match match_input(&wanted, &inputs) {
                        Ok(target) => target,
                        Err(error) => return controller_error(error),
                    };
                    if target.current {
                        return controller_complete(
                            object_value(json!({"changed": false, "input": target.cname})),
                            false,
                        );
                    }
                    self.enqueue(
                        "getCurrentInput",
                        Map::new(),
                        PendingWorkflow::InputFresh {
                            cname: target.cname.clone(),
                        },
                    )
                } else {
                    controller_complete(object_value(json!({"inputs": inputs})), false)
                }
            }
            PendingWorkflow::InputFresh { cname } => match current_input_state(&response) {
                Ok((_, hash_value)) => {
                    let mut fields = Map::new();
                    fields.insert("cname".into(), Value::String(cname));
                    fields.insert("hashValue".into(), Value::from(hash_value));
                    self.enqueue("setInput", fields, PendingWorkflow::InputWrite)
                }
                Err(error) => controller_error(error),
            },
            PendingWorkflow::InputWrite => {
                controller_complete(object_value(json!({"changed": true})), false)
            }
            PendingWorkflow::SettingRead {
                category,
                name,
                mutation,
                retried,
            } => {
                let setting = match parse_setting(&response, &name) {
                    Ok(setting) => setting,
                    Err(error) => return controller_error(error),
                };
                if let SettingMutation::Modify(value) = &mutation
                    && let Err(error) = validate_setting_snapshot(value, &setting)
                {
                    return controller_error(error);
                }
                let mut fields = setting_fields(&category, &name);
                fields.insert("hashValue".into(), Value::from(setting.hash_value));
                let operation = match &mutation {
                    SettingMutation::Modify(value) => {
                        fields.insert("value".into(), value.clone());
                        "setSetting"
                    }
                    SettingMutation::Action => "triggerSetting",
                };
                self.enqueue(
                    operation,
                    fields,
                    PendingWorkflow::SettingWrite {
                        category,
                        name,
                        mutation,
                        retried,
                    },
                )
            }
            PendingWorkflow::SettingWrite { retried, .. } => controller_complete(
                object_value(json!({"changed": true, "retried": retried})),
                false,
            ),
        }
    }

    pub fn reject(&mut self, request_id: u32) -> VizioControllerOutput {
        if self
            .pending
            .as_ref()
            .is_some_and(|(expected, _)| *expected == request_id)
        {
            self.pending = None;
            controller_error(failure(
                VizioFailureKind::Transport,
                "SmartCast transport failed",
                true,
            ))
        } else {
            controller_error(failure(
                VizioFailureKind::InvalidInput,
                "Stale SmartCast response was ignored",
                false,
            ))
        }
    }

    pub fn cancel(&mut self) {
        self.pending = None;
    }

    pub fn auth_token(&self) -> Option<String> {
        self.config.auth_token.clone()
    }

    pub fn clear_auth_token(&mut self) {
        self.config.auth_token = None;
        self.pending = None;
    }

    fn enqueue(
        &mut self,
        operation: &str,
        fields: Map<String, Value>,
        pending: PendingWorkflow,
    ) -> VizioControllerOutput {
        let input = RequestInput {
            host: self.config.host.clone(),
            auth_token: self.config.auth_token.clone(),
            device_id: self.config.device_id.clone(),
            device_name: self.config.device_name.clone(),
            timeout_millis: self.config.timeout_millis,
            max_response_bytes: self.config.max_response_bytes,
            fields,
        };
        match request_for(operation, input) {
            Ok(request) => {
                let request_id = self.next_request_id;
                self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
                self.pending = Some((request_id, pending));
                VizioControllerOutput {
                    kind: VizioControllerOutputKind::Request,
                    request_id: Some(request_id),
                    request: Some(request),
                    result: None,
                    error: None,
                    credential_changed: false,
                }
            }
            Err(error) => controller_error(error),
        }
    }
}
fn controller_error(error: VizioFailure) -> VizioControllerOutput {
    VizioControllerOutput {
        kind: VizioControllerOutputKind::Error,
        request_id: None,
        request: None,
        result: None,
        error: Some(error),
        credential_changed: false,
    }
}

fn controller_complete(
    result: Option<JsonObject>,
    credential_changed: bool,
) -> VizioControllerOutput {
    VizioControllerOutput {
        kind: VizioControllerOutputKind::Complete,
        request_id: None,
        request: None,
        result,
        error: None,
        credential_changed,
    }
}
