/// Shared enforcement helper for model-access policy (Unified API path).
///
/// Model access policy is only enforced here (Unified API) because the
/// request model string is not available at the outer middleware layer for
/// Router / Direct Proxy paths.
use http::Extensions;

use crate::{
    error::{api::ApiError, invalid_req::InvalidRequestError},
    types::extensions::VkPolicy,
    virtual_key::model_policy::model_access_allowed,
};

/// Checks whether the requested model is permitted by the virtual key's
/// `allowed_models` / `blocked_models` policy.
///
/// Returns `Ok(())` when:
/// - `VkPolicy` is absent from `extensions` (non-VK authenticated request), or
/// - the model is allowed by the policy.
///
/// Returns `Err(ApiError::InvalidRequest(ModelAccessDenied))` (HTTP 403) when
/// the model is explicitly blocked.
///
/// The caller is responsible for emitting metrics/logs on denial.
pub fn check_model_access(
    extensions: &Extensions,
    model_str: &str,
) -> Result<(), ApiError> {
    let Some(policy) = extensions.get::<VkPolicy>() else {
        return Ok(());
    };

    if !model_access_allowed(
        model_str,
        policy.allowed_models.as_deref(),
        policy.blocked_models.as_deref(),
    ) {
        return Err(ApiError::InvalidRequest(
            InvalidRequestError::ModelAccessDenied(model_str.to_string()),
        ));
    }

    Ok(())
}
