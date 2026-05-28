//! Tests for workspace provider allowlist enforcement (F-10).
//!
//! Pure-function tests exercise `AppState::is_provider_allowed_for_workspace`
//! directly using a lightweight test app (no DB required).
use std::collections::HashSet;

use ai_gateway::{
    app::build_test_app, app_state::WorkspaceProviderAllowlist, config::Config,
    tests::TestDefault, types::provider::InferenceProvider,
};
use rustc_hash::FxHashMap;
use uuid::Uuid;

// ─── Pure-function tests for is_provider_allowed_for_workspace ─────────────

fn make_allowlist(
    workspace_id: Uuid,
    providers: Vec<InferenceProvider>,
) -> WorkspaceProviderAllowlist {
    let mut map: FxHashMap<Uuid, HashSet<InferenceProvider>> =
        FxHashMap::default();
    map.insert(workspace_id, providers.into_iter().collect());
    map
}

/// Build a minimal `AppState` with a pre-loaded allowlist (no DB needed).
async fn state_with_allowlist(
    allowlist: WorkspaceProviderAllowlist,
) -> ai_gateway::app_state::AppState {
    let config = Config::test_default();
    let app = build_test_app(config).await.expect("build_test_app");
    app.state.set_workspace_provider_allowlist(allowlist);
    app.state.clone()
}

#[tokio::test]
async fn empty_allowlist_allows_all_providers() {
    let state = state_with_allowlist(FxHashMap::default()).await;
    let ws = Uuid::new_v4();
    assert!(
        state.is_provider_allowed_for_workspace(ws, &InferenceProvider::OpenAI)
    );
    assert!(
        state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );
}

#[tokio::test]
async fn workspace_not_in_allowlist_allows_all_providers() {
    let present = Uuid::new_v4();
    let absent = Uuid::new_v4();
    let allowlist = make_allowlist(present, vec![InferenceProvider::OpenAI]);
    let state = state_with_allowlist(allowlist).await;

    assert!(state.is_provider_allowed_for_workspace(
        present,
        &InferenceProvider::OpenAI
    ));
    assert!(!state.is_provider_allowed_for_workspace(
        present,
        &InferenceProvider::Anthropic
    ));

    assert!(
        state.is_provider_allowed_for_workspace(
            absent,
            &InferenceProvider::OpenAI
        )
    );
    assert!(state.is_provider_allowed_for_workspace(
        absent,
        &InferenceProvider::Anthropic
    ));
}

#[tokio::test]
async fn workspace_with_explicit_allowlist_blocks_other_providers() {
    let ws = Uuid::new_v4();
    let allowlist = make_allowlist(ws, vec![InferenceProvider::OpenAI]);
    let state = state_with_allowlist(allowlist).await;

    assert!(
        state.is_provider_allowed_for_workspace(ws, &InferenceProvider::OpenAI)
    );
    assert!(
        !state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );
}

#[tokio::test]
async fn workspace_allowlist_with_multiple_providers() {
    let ws = Uuid::new_v4();
    let allowlist = make_allowlist(
        ws,
        vec![InferenceProvider::OpenAI, InferenceProvider::Anthropic],
    );
    let state = state_with_allowlist(allowlist).await;

    assert!(
        state.is_provider_allowed_for_workspace(ws, &InferenceProvider::OpenAI)
    );
    assert!(
        state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );
    assert!(!state.is_provider_allowed_for_workspace(
        ws,
        &InferenceProvider::Named("mistral".into())
    ));
}

// ─── Allowlist hot-swap test ───────────────────────────────────────────────

#[tokio::test]
async fn allowlist_hot_swap_is_visible_immediately() {
    let ws = Uuid::new_v4();

    let app = build_test_app(Config::test_default())
        .await
        .expect("build_test_app");
    let state = app.state.clone();

    assert!(
        state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );

    let allowlist = make_allowlist(ws, vec![InferenceProvider::OpenAI]);
    state.set_workspace_provider_allowlist(allowlist);

    assert!(
        state.is_provider_allowed_for_workspace(ws, &InferenceProvider::OpenAI)
    );
    assert!(
        !state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );

    let allowlist = make_allowlist(
        ws,
        vec![InferenceProvider::OpenAI, InferenceProvider::Anthropic],
    );
    state.set_workspace_provider_allowlist(allowlist);
    assert!(
        state.is_provider_allowed_for_workspace(
            ws,
            &InferenceProvider::Anthropic
        )
    );
}
