use crate::agent::context::AgentTrustLevel;

pub const SOURCE_VIRTUAL_KEY_LABEL: &str = "virtual_key_label";
pub const SOURCE_SELF_REPORTED_EVENT: &str = "self_reported_event";
pub const SOURCE_SELF_REPORTED_HEADER: &str = "self_reported_header";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNameConflict {
    pub registered_agent_name: String,
    pub self_reported_agent_name: String,
    pub self_reported_agent_name_source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentName {
    pub name: Option<String>,
    pub source: Option<&'static str>,
    pub trust_level: Option<AgentTrustLevel>,
    pub conflict: Option<AgentNameConflict>,
}

#[must_use]
pub fn parse_agent_name_from_vk_label(label: &str) -> Option<String> {
    let (prefix, name) = label.split_once(':')?;
    if !prefix.trim().eq_ignore_ascii_case("agent") {
        return None;
    }
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

#[must_use]
pub fn resolve_agent_name(
    registered_agent_name: Option<&str>,
    event_agent_name: Option<&str>,
    header_agent_name: Option<&str>,
) -> ResolvedAgentName {
    let registered = nonempty_trimmed(registered_agent_name);
    let event = nonempty_trimmed(event_agent_name);
    let header = nonempty_trimmed(header_agent_name);

    let self_reported = event
        .map(|name| (name, SOURCE_SELF_REPORTED_EVENT))
        .or_else(|| header.map(|name| (name, SOURCE_SELF_REPORTED_HEADER)));

    if let Some(registered) = registered {
        let conflict = self_reported.and_then(|(self_name, source)| {
            (self_name != registered).then(|| AgentNameConflict {
                registered_agent_name: registered.to_string(),
                self_reported_agent_name: self_name.to_string(),
                self_reported_agent_name_source: source,
            })
        });
        return ResolvedAgentName {
            name: Some(registered.to_string()),
            source: Some(SOURCE_VIRTUAL_KEY_LABEL),
            trust_level: Some(AgentTrustLevel::AuthBound),
            conflict,
        };
    }

    if let Some((name, source)) = self_reported {
        return ResolvedAgentName {
            name: Some(name.to_string()),
            source: Some(source),
            trust_level: Some(AgentTrustLevel::SelfReported),
            conflict: None,
        };
    }

    ResolvedAgentName {
        name: None,
        source: None,
        trust_level: None,
        conflict: None,
    }
}

fn nonempty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registered_agent_name_from_vk_label() {
        assert_eq!(
            parse_agent_name_from_vk_label("agent:Test Agent Free VK 3")
                .as_deref(),
            Some("Test Agent Free VK 3")
        );
        assert_eq!(
            parse_agent_name_from_vk_label("agent:Team:A Bot").as_deref(),
            Some("Team:A Bot")
        );
        assert_eq!(
            parse_agent_name_from_vk_label(" agent :  Support Bot  ")
                .as_deref(),
            Some("Support Bot")
        );
        assert_eq!(
            parse_agent_name_from_vk_label("AGENT:Support Bot").as_deref(),
            Some("Support Bot")
        );
    }

    #[test]
    fn ignores_non_agent_or_empty_vk_labels() {
        assert!(parse_agent_name_from_vk_label("agent:").is_none());
        assert!(parse_agent_name_from_vk_label("agent:    ").is_none());
        assert!(parse_agent_name_from_vk_label("member:Alice").is_none());
        assert!(parse_agent_name_from_vk_label("team:Support").is_none());
        assert!(parse_agent_name_from_vk_label("plain label").is_none());
    }

    #[test]
    fn resolves_registered_name_before_self_reported_names() {
        let resolved = resolve_agent_name(
            Some("Support Bot"),
            Some("Payload Bot"),
            Some("Header Bot"),
        );

        assert_eq!(resolved.name.as_deref(), Some("Support Bot"));
        assert_eq!(resolved.source, Some(SOURCE_VIRTUAL_KEY_LABEL));
        assert_eq!(resolved.trust_level, Some(AgentTrustLevel::AuthBound));
        assert_eq!(
            resolved.conflict,
            Some(AgentNameConflict {
                registered_agent_name: "Support Bot".to_string(),
                self_reported_agent_name: "Payload Bot".to_string(),
                self_reported_agent_name_source: SOURCE_SELF_REPORTED_EVENT,
            })
        );
    }

    #[test]
    fn resolves_event_name_before_header_name_without_registered_name() {
        let resolved =
            resolve_agent_name(None, Some("Payload Bot"), Some("Header Bot"));

        assert_eq!(resolved.name.as_deref(), Some("Payload Bot"));
        assert_eq!(resolved.source, Some(SOURCE_SELF_REPORTED_EVENT));
        assert_eq!(resolved.trust_level, Some(AgentTrustLevel::SelfReported));
        assert_eq!(resolved.conflict, None);
    }

    #[test]
    fn resolves_header_name_when_no_registered_or_event_name() {
        let resolved = resolve_agent_name(None, None, Some("Header Bot"));

        assert_eq!(resolved.name.as_deref(), Some("Header Bot"));
        assert_eq!(resolved.source, Some(SOURCE_SELF_REPORTED_HEADER));
        assert_eq!(resolved.trust_level, Some(AgentTrustLevel::SelfReported));
        assert_eq!(resolved.conflict, None);
    }
}
