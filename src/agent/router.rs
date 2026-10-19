//! Deterministic model selection by name or profile, with fallback before output.

use std::collections::BTreeMap;
use std::fmt;

use super::{
    Model, ModelCapabilities, ModelError, ModelMode, ModelRequest, ModelResponse, ModelSelection,
    ModelStreamEvent, RouteRequest,
};

/// Errors from registering models or resolving a route to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    NoModels,
    NoRoute {
        mode: ModelMode,
    },
    MissingModel {
        model: String,
    },
    MissingCapability {
        model: String,
        capability: &'static str,
    },
    DuplicateModel {
        model: String,
    },
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModels => formatter.write_str("no models are registered"),
            Self::NoRoute { mode } => {
                write!(formatter, "no model route is configured for {mode:?}")
            }
            Self::MissingModel { model } => write!(formatter, "model is not registered: {model}"),
            Self::MissingCapability { model, capability } => {
                write!(formatter, "model '{model}' does not support {capability}")
            }
            Self::DuplicateModel { model } => {
                write!(formatter, "model is already registered: {model}")
            }
        }
    }
}

impl std::error::Error for RouteError {}

struct RegisteredModel {
    capabilities: ModelCapabilities,
    model: Box<dyn Model>,
}

/// Deterministic model selection with capability checks and safe fallback.
pub struct ModelRouter {
    models: BTreeMap<String, RegisteredModel>,
    default_model: Option<String>,
    fast_model: Option<String>,
    strong_model: Option<String>,
    fallback_model: Option<String>,
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRouter {
    pub fn new() -> Self {
        Self {
            models: BTreeMap::new(),
            default_model: None,
            fast_model: None,
            strong_model: None,
            fallback_model: None,
        }
    }

    pub fn register_model<M>(
        &mut self,
        name: impl Into<String>,
        capabilities: ModelCapabilities,
        model: M,
    ) -> Result<(), RouteError>
    where
        M: Model + 'static,
    {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(RouteError::MissingModel { model: name });
        }
        if self.models.contains_key(&name) {
            return Err(RouteError::DuplicateModel { model: name });
        }
        self.models.insert(
            name,
            RegisteredModel {
                capabilities,
                model: Box::new(model),
            },
        );
        Ok(())
    }

    pub fn set_default(&mut self, model: impl Into<String>) -> Result<(), RouteError> {
        let model = self.checked_model(model)?;
        self.default_model = Some(model);
        Ok(())
    }

    pub fn set_fast(&mut self, model: impl Into<String>) -> Result<(), RouteError> {
        let model = self.checked_model(model)?;
        self.fast_model = Some(model);
        Ok(())
    }

    pub fn set_strong(&mut self, model: impl Into<String>) -> Result<(), RouteError> {
        let model = self.checked_model(model)?;
        self.strong_model = Some(model);
        Ok(())
    }

    pub fn set_fallback(&mut self, model: impl Into<String>) -> Result<(), RouteError> {
        let model = self.checked_model(model)?;
        self.fallback_model = Some(model);
        Ok(())
    }

    pub fn model_names(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }

    pub fn select(&self, request: &RouteRequest) -> Result<ModelSelection, RouteError> {
        if self.models.is_empty() {
            return Err(RouteError::NoModels);
        }
        for model in self.candidate_names(request) {
            if let Ok(entry) = self.valid_entry(&model, request) {
                return Ok(ModelSelection {
                    model,
                    capabilities: entry.capabilities,
                });
            }
        }
        Err(self.last_route_error(request))
    }

    fn checked_model(&self, model: impl Into<String>) -> Result<String, RouteError> {
        let model = model.into();
        if !self.models.contains_key(&model) {
            return Err(RouteError::MissingModel { model });
        }
        Ok(model)
    }

    fn candidate_names(&self, request: &RouteRequest) -> Vec<String> {
        let preferred = request
            .requested_model
            .clone()
            .or_else(|| match request.mode {
                ModelMode::Default => self.default_model.clone(),
                ModelMode::Fast => self
                    .fast_model
                    .clone()
                    .or_else(|| self.default_model.clone()),
                ModelMode::Strong => self
                    .strong_model
                    .clone()
                    .or_else(|| self.default_model.clone()),
            });
        let mut candidates = Vec::new();
        if let Some(model) = preferred {
            candidates.push(model);
        }
        if let Some(model) = &self.fallback_model
            && !candidates.iter().any(|candidate| candidate == model)
        {
            candidates.push(model.clone());
        }
        candidates
    }

    fn valid_entry(
        &self,
        model: &str,
        request: &RouteRequest,
    ) -> Result<&RegisteredModel, RouteError> {
        let entry = self
            .models
            .get(model)
            .ok_or_else(|| RouteError::MissingModel {
                model: model.to_owned(),
            })?;
        if request.requires_tools && !entry.capabilities.tools {
            return Err(RouteError::MissingCapability {
                model: model.to_owned(),
                capability: "tool calls",
            });
        }
        if request.requires_streaming && !entry.capabilities.streaming {
            return Err(RouteError::MissingCapability {
                model: model.to_owned(),
                capability: "streaming",
            });
        }
        Ok(entry)
    }

    fn last_route_error(&self, request: &RouteRequest) -> RouteError {
        let candidates = self.candidate_names(request);
        if let Some(model) = candidates.first()
            && let Some(error) = self.valid_entry(model, request).err()
        {
            return error;
        }
        RouteError::NoRoute { mode: request.mode }
    }
}

impl Model for ModelRouter {
    fn set_cancel_token(&mut self, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        for entry in self.models.values_mut() {
            entry.model.set_cancel_token(cancel.clone());
        }
    }

    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let candidates = self.candidate_names(&request.route);
        if candidates.is_empty() {
            return Err(ModelError::new(
                self.last_route_error(&request.route).to_string(),
            ));
        }

        let mut last_error = None;
        for model in candidates {
            if let Err(error) = self.valid_entry(&model, &request.route) {
                last_error = Some(error.to_string());
                continue;
            }
            let result = self
                .models
                .get_mut(&model)
                .expect("validated model must remain registered")
                .model
                .respond(request);
            match result {
                Ok(response) => return Ok(response),
                Err(error) => last_error = Some(format!("{model}: {error}")),
            }
        }
        Err(ModelError::new(format!(
            "all model routes failed: {}",
            last_error.unwrap_or_else(|| self.last_route_error(&request.route).to_string())
        )))
    }

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        let candidates = self.candidate_names(&request.route);
        if candidates.is_empty() {
            return Err(ModelError::new(
                self.last_route_error(&request.route).to_string(),
            ));
        }

        let mut last_error = None;
        for model in candidates {
            if let Err(error) = self.valid_entry(&model, &request.route) {
                last_error = Some(error.to_string());
                continue;
            }
            let mut emitted = false;
            let result = {
                let entry = self
                    .models
                    .get_mut(&model)
                    .expect("validated model must remain registered");
                entry.model.respond_stream(request, &mut |event| {
                    emitted = true;
                    emit(event);
                })
            };
            match result {
                Ok(response) => return Ok(response),
                Err(error) if emitted => return Err(error),
                Err(error) => last_error = Some(format!("{model}: {error}")),
            }
        }
        Err(ModelError::new(format!(
            "all model routes failed: {}",
            last_error.unwrap_or_else(|| self.last_route_error(&request.route).to_string())
        )))
    }
}
