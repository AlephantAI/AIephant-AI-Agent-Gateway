use ai_gateway::virtual_key::model_policy::model_access_allowed;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|&s| s.to_string()).collect()
}

struct Case {
    label: &'static str,
    model: &'static str,
    allowed: Option<Vec<String>>,
    blocked: Option<Vec<String>>,
    want: bool,
}

#[test]
#[allow(clippy::too_many_lines)]
fn model_policy_table() {
    let cases = vec![
        Case {
            label: "both None -> allow",
            model: "gpt-4",
            allowed: None,
            blocked: None,
            want: true,
        },
        Case {
            label: "both empty vec -> allow",
            model: "gpt-4",
            allowed: Some(vec![]),
            blocked: Some(vec![]),
            want: true,
        },
        Case {
            label: "model in allowed, no blocked -> allow",
            model: "gpt-4",
            allowed: Some(s(&["gpt-4"])),
            blocked: None,
            want: true,
        },
        Case {
            label: "model NOT in allowed, no blocked -> allow (rule 3)",
            model: "claude-3",
            allowed: Some(s(&["gpt-4"])),
            blocked: None,
            want: true,
        },
        Case {
            label: "model in blocked, no allowed -> deny",
            model: "gpt-3.5-turbo",
            allowed: None,
            blocked: Some(s(&["gpt-3.5-turbo"])),
            want: false,
        },
        Case {
            label: "model NOT in blocked, no allowed -> allow (rule 3)",
            model: "gpt-4",
            allowed: None,
            blocked: Some(s(&["gpt-3.5-turbo"])),
            want: true,
        },
        Case {
            label: "model in both allowed and blocked -> allowed wins (rule 1)",
            model: "gpt-4",
            allowed: Some(s(&["gpt-4"])),
            blocked: Some(s(&["gpt-4"])),
            want: true,
        },
        Case {
            label: "model in neither list -> allow (rule 3)",
            model: "claude-3-opus",
            allowed: Some(s(&["gpt-4"])),
            blocked: Some(s(&["gpt-3.5-turbo"])),
            want: true,
        },
        Case {
            label: "allowed match is case-insensitive (DB upper, req lower)",
            model: "gpt-4",
            allowed: Some(s(&["GPT-4"])),
            blocked: None,
            want: true,
        },
        Case {
            label: "allowed match is case-insensitive (DB lower, req mixed)",
            model: "GPT-4",
            allowed: Some(s(&["gpt-4"])),
            blocked: None,
            want: true,
        },
        Case {
            label: "blocked match is case-insensitive (DB upper, req lower)",
            model: "gpt-3.5-turbo",
            allowed: None,
            blocked: Some(s(&["GPT-3.5-TURBO"])),
            want: false,
        },
        Case {
            label: "case-insensitive: allowed beats blocked when both match \
                    mixed case",
            model: "Claude-3-Opus",
            allowed: Some(s(&["claude-3-opus"])),
            blocked: Some(s(&["CLAUDE-3-OPUS"])),
            want: true,
        },
        Case {
            label: "model matches second item in allowed list",
            model: "claude-3",
            allowed: Some(s(&["gpt-4", "claude-3"])),
            blocked: None,
            want: true,
        },
        Case {
            label: "model matches second item in blocked list",
            model: "claude-3",
            allowed: None,
            blocked: Some(s(&["gpt-4", "claude-3"])),
            want: false,
        },
        Case {
            label: "exact match only: blocked gpt-4o does not block gpt-4",
            model: "gpt-4",
            allowed: None,
            blocked: Some(s(&["gpt-4o"])),
            want: true,
        },
        Case {
            label: "exact match only: allowed gpt-4o does not override \
                    blocked gpt-4",
            model: "gpt-4",
            allowed: Some(s(&["gpt-4o"])),
            blocked: Some(s(&["gpt-4"])),
            want: false,
        },
    ];

    for c in cases {
        let result = model_access_allowed(c.model, c.allowed.as_deref(), c.blocked.as_deref());
        assert_eq!(
            result, c.want,
            "FAILED: '{}' — model='{}', allowed={:?}, blocked={:?}",
            c.label, c.model, c.allowed, c.blocked
        );
    }
}
