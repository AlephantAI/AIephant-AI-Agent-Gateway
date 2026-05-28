use uuid::Uuid;

use crate::{
    app_state::AppState,
    error::{api::ApiError, internal::InternalError},
    types::{extensions::AuthContext, provider::InferenceProvider},
};

pub(super) fn enforce_workspace_provider_allowlist(
    app_state: &AppState,
    auth_ctx: Option<&AuthContext>,
    target_provider: &InferenceProvider,
) -> Result<(), ApiError> {
    let Some(workspace_id) = allowlist_workspace_id_for_request(auth_ctx)
    else {
        return Ok(());
    };

    // F-10: enforce workspace provider allowlist in Cloud mode.
    if !app_state
        .is_provider_allowed_for_workspace(workspace_id, target_provider)
    {
        tracing::warn!(
            provider = %target_provider,
            workspace_id = %workspace_id,
            "provider not in workspace allowlist — rejecting request (F-10)"
        );
        crate::fallback::observability::log_decision(
            &app_state.config().fallback_policy,
            crate::fallback::observability::DecisionKind::ProviderDenied,
            None,
            target_provider,
        );
        return Err(InternalError::ProviderNotAllowedForWorkspace(
            target_provider.clone(),
        )
        .into());
    }
    Ok(())
}

pub(super) fn allowlist_workspace_id_for_request(
    auth_ctx: Option<&AuthContext>,
) -> Option<Uuid> {
    auth_ctx.map(|ctx| *ctx.org_id.as_ref())
}
