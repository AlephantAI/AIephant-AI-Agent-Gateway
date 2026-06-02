//! Estimated input tokens and micro-USD for `PolicyService/Evaluate`.

use std::str::FromStr as _;

use bytes::Bytes;
use http::{Extensions, HeaderMap};

use crate::{
    app_state::AppState,
    logger::model_info::{lookup_model_info, policy_estimated_input_micro_usd},
    middleware::large_context::{
        headers::ALEPHANT_MODEL_OVERRIDE_HEADER,
        heuristics::{estimate_input_tokens, resolve_primary_model},
        parse::parse_chat_completions_payload,
    },
    policy_proto::EvaluateRequest,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[must_use]
fn model_override_from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(ALEPHANT_MODEL_OVERRIDE_HEADER)?;
    let v = raw.to_str().ok()?.trim();
    if v.is_empty() {
        return None;
    }
    // Require `provider/model` shape so legacy placeholder values like `m`
    // are ignored (see `model_override_ignores_legacy_alephant_header`).
    if !v.contains('/') {
        return None;
    }
    Some(v.to_string())
}

#[must_use]
fn model_id_for_catalog(provider: &InferenceProvider, model_str: &str) -> ModelId {
    if model_str.contains('/') {
        return ModelId::from_str(model_str)
            .unwrap_or_else(|_| ModelId::Unknown(model_str.to_string()));
    }
    ModelId::from_str_and_provider(provider.clone(), model_str)
        .unwrap_or_else(|_| ModelId::Unknown(model_str.to_string()))
}

pub(crate) async fn fill_evaluate_request_estimates(
    app_state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    body: &Bytes,
    grpc_req: &mut EvaluateRequest,
) {
    grpc_req.estimated_input_tokens = 0;
    grpc_req.estimated_input_usd = 0.0;

    let Some(provider) = extensions.get::<InferenceProvider>() else {
        return;
    };

    let model_override = model_override_from_headers(headers);

    let Ok(Some(payload)) = parse_chat_completions_payload(body.as_ref()) else {
        return;
    };

    let body_model = payload.model.as_deref();
    let Some(primary_model) = resolve_primary_model(body_model, model_override.as_deref()) else {
        return;
    };

    let Some(estimated_tokens) = estimate_input_tokens(&payload, &primary_model, Some(provider))
    else {
        return;
    };

    let model_id = model_id_for_catalog(provider, &primary_model);
    let Some(info) = lookup_model_info(app_state, provider, &model_id).await else {
        grpc_req.estimated_input_tokens = estimated_tokens;
        return;
    };

    grpc_req.estimated_input_tokens = estimated_tokens;
    grpc_req.estimated_input_usd = policy_estimated_input_micro_usd(estimated_tokens, &info);
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::model_override_from_headers;

    #[test]
    fn model_override_prefers_alephant_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Alephant-Model-Override",
            http::HeaderValue::from_static("openai/gpt-4o-mini"),
        );
        assert_eq!(
            model_override_from_headers(&headers).as_deref(),
            Some("openai/gpt-4o-mini"),
        );
    }

    #[test]
    fn model_override_ignores_legacy_alephant_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Alephant-Model-Override",
            http::HeaderValue::from_static("m"),
        );
        assert_eq!(model_override_from_headers(&headers), None);
    }
}
