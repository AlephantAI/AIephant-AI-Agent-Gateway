//! Client ecosystem profile resolution and native semantic passthrough
//! pairing (see
//! `docs/plans/2026-05-14-ide-client-adapter-native-passthrough-*`).

use crate::{
    endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI},
    types::provider::InferenceProvider,
};

/// Stable client ecosystem label for mapper routing / observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientProfile {
    #[default]
    Unknown,
    ClaudeCode,
    CodexCli,
    CursorIde,
    GithubCopilot,
    Cline,
    OpenClaw,
    Hermes,
}

impl ClientProfile {
    #[must_use]
    pub const fn as_otel_label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ClaudeCode => "claude_code",
            Self::CodexCli => "codex_cli",
            Self::CursorIde => "cursor_ide",
            Self::GithubCopilot => "github_copilot",
            Self::Cline => "cline",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
        }
    }
}

/// Result of header / heuristic client profile resolution for one request.
#[derive(Debug, Clone)]
pub struct ClientProfileResolution {
    pub profile: ClientProfile,
    pub from_explicit_header: bool,
}

const CLIENT_PROFILE_HEADER: &str = "alephant-client-profile";

/// Resolves [`ClientProfile`] from headers: explicit header wins; optional
/// heuristic from `User-Agent` and Cursor fingerprint headers; on
/// disagreement logs `warn` and keeps explicit.
#[must_use]
pub fn resolve_client_profile(headers: &http::HeaderMap) -> ClientProfileResolution {
    let explicit = headers
        .get(CLIENT_PROFILE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let heuristic = heuristic_from_headers(headers);

    match (explicit, heuristic) {
        (Some(e), Some(h)) if profile_from_str(e) != h => {
            tracing::warn!(
                explicit = e,
                ?h,
                "client profile: explicit header disagrees with heuristic; \
                 using explicit"
            );
            ClientProfileResolution {
                profile: profile_from_str(e),
                from_explicit_header: true,
            }
        }
        (Some(e), _) => ClientProfileResolution {
            profile: profile_from_str(e),
            from_explicit_header: true,
        },
        (None, Some(h)) => ClientProfileResolution {
            profile: h,
            from_explicit_header: false,
        },
        (None, None) => ClientProfileResolution {
            profile: ClientProfile::Unknown,
            from_explicit_header: false,
        },
    }
}

fn is_codex_stack(headers: &http::HeaderMap, ua: &str) -> bool {
    if ua.contains("codex-cli") || ua.contains("codex_vscode") {
        return true;
    }
    if headers.keys().any(|name| {
        name.as_str()
            .as_bytes()
            .get(..b"x-codex-".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"x-codex-"))
    }) {
        return true;
    }
    headers
        .get(http::HeaderName::from_static("originator"))
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("codex_"))
}

fn heuristic_from_headers(headers: &http::HeaderMap) -> Option<ClientProfile> {
    let ua = headers
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ua.contains("claude-cli") || ua.contains("claude-code") {
        return Some(ClientProfile::ClaudeCode);
    }
    if is_codex_stack(headers, &ua) {
        return Some(ClientProfile::CodexCli);
    }
    if ua.contains("githubcopilotchat") {
        return Some(ClientProfile::GithubCopilot);
    }
    // Cursor IDE / Cursor stack: desktop often sends `Cursor/*` User-Agent;
    // 9router `open-sse/utils/cursorChecksum.js` uses `connect-es/*` when
    // calling Cursor API. Also accept any `x-cursor-*` request header
    // (checksum, client-version, etc.) per that module's outbound header set.
    if ua.contains("cursor")
        || ua.contains("connect-es")
        || headers.keys().any(|name| {
            name.as_str()
                .as_bytes()
                .get(..b"x-cursor-".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"x-cursor-"))
        })
    {
        return Some(ClientProfile::CursorIde);
    }
    None
}

fn profile_from_str(s: &str) -> ClientProfile {
    match s.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => ClientProfile::ClaudeCode,
        "codex" => ClientProfile::CodexCli,
        "cursor" => ClientProfile::CursorIde,
        "copilot" | "github-copilot" => ClientProfile::GithubCopilot,
        "cline" => ClientProfile::Cline,
        "openclaw" => ClientProfile::OpenClaw,
        "hermes" => ClientProfile::Hermes,
        _ => ClientProfile::Unknown,
    }
}

/// Returns true iff the design (client ecosystem ⊕ upstream) allows native
/// semantic passthrough **and** source/target are on the same wire family
/// (Phase 1 guard).
#[must_use]
pub fn native_semantic_passthrough(
    profile: ClientProfile,
    source: &ApiEndpoint,
    target: &ApiEndpoint,
    target_provider: &InferenceProvider,
) -> bool {
    if !endpoints_same_wire_family(source, target) {
        return false;
    }
    match profile {
        ClientProfile::ClaudeCode => {
            matches!(target_provider, InferenceProvider::Anthropic)
        }
        ClientProfile::CodexCli => {
            matches!(target_provider, InferenceProvider::OpenAI)
        }
        ClientProfile::Unknown => false,
        ClientProfile::OpenClaw
        | ClientProfile::Hermes
        | ClientProfile::CursorIde
        | ClientProfile::Cline
        | ClientProfile::GithubCopilot => false,
    }
}

pub(crate) fn endpoints_same_wire_family(source: &ApiEndpoint, target: &ApiEndpoint) -> bool {
    matches!(
        (source, target),
        (
            ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)),
            ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)),
        ) | (
            ApiEndpoint::OpenAI(OpenAI::Responses(_)),
            ApiEndpoint::OpenAI(OpenAI::Responses(_)),
        ) | (
            ApiEndpoint::Anthropic(Anthropic::Messages(_)),
            ApiEndpoint::Anthropic(Anthropic::Messages(_)),
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI};

    #[test]
    fn native_semantic_passthrough_claude_requires_anthropic_and_same_wire() {
        let src = ApiEndpoint::Anthropic(Anthropic::messages());
        let tgt = ApiEndpoint::Anthropic(Anthropic::messages());
        assert!(native_semantic_passthrough(
            ClientProfile::ClaudeCode,
            &src,
            &tgt,
            &InferenceProvider::Anthropic,
        ));
        assert!(!native_semantic_passthrough(
            ClientProfile::ClaudeCode,
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::Anthropic(Anthropic::messages()),
            &InferenceProvider::Anthropic,
        ));
    }

    #[test]
    fn native_semantic_passthrough_codex_requires_openai_provider() {
        let src = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let tgt = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        assert!(native_semantic_passthrough(
            ClientProfile::CodexCli,
            &src,
            &tgt,
            &InferenceProvider::OpenAI,
        ));
        assert!(!native_semantic_passthrough(
            ClientProfile::CodexCli,
            &src,
            &tgt,
            &InferenceProvider::Anthropic,
        ));
    }

    #[test]
    fn resolve_explicit_over_heuristic() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("alephant-client-profile"),
            http::HeaderValue::from_static("codex"),
        );
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("claude-cli/1.0"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CodexCli);
        assert!(r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_cursor_connect_es_user_agent() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("connect-es/1.6.1"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CursorIde);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_cursor_slash_version_user_agent() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("Cursor/1.0"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CursorIde);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_cursor_x_cursor_header_without_ua() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("x-cursor-client-version"),
            http::HeaderValue::from_static("0.0.0"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CursorIde);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_codex_vscode_user_agent() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("codex_vscode/0.131.0-alpha.9 (Ubuntu 22.4.0; x86_64)"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CodexCli);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_codex_x_codex_header_without_ua() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("x-codex-window-id"),
            http::HeaderValue::from_static("abc"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CodexCli);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_codex_originator() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::HeaderName::from_static("originator"),
            http::HeaderValue::from_static("codex_vscode"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CodexCli);
        assert!(!r.from_explicit_header);
    }

    #[test]
    fn resolve_heuristic_cursor_not_codex() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("Cursor/1.0"),
        );
        let r = resolve_client_profile(&headers);
        assert_eq!(r.profile, ClientProfile::CursorIde);
        assert!(!r.from_explicit_header);
    }
}
