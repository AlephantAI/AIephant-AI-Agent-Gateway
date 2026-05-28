//! Policy fallback, candidates, and deterministic tie-break (max Unicode
//! order).

use serde::Deserialize;
use serde_json::Value;

/// Parse from `policy_configs.config` `allowedModels` (and legacy shapes), or
/// from `policy_overrides.overrides` `modelWhitelist.models` (and legacy
/// shapes).

#[derive(Debug, Deserialize)]
struct ModelListDoc {
    #[serde(default, alias = "models")]
    allowed_models: Option<Vec<String>>,
}

fn non_empty_string_ids(a: Value) -> Option<Vec<String>> {
    let v: Vec<String> = a
        .as_array()?
        .iter()
        .filter_map(|e| e.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() { None } else { Some(v) }
}

/// `policy_configs.config`：`"allowedModels": [ "a", "b" ]`。
#[must_use]
pub fn model_ids_from_config_json(v: &Value) -> Option<Vec<String>> {
    if let Some(x) = v
        .get("allowedModels")
        .or_else(|| v.get("allowed_models"))
        .cloned()
        && let Some(list) = non_empty_string_ids(x)
    {
        return Some(list);
    }
    let doc: ModelListDoc = serde_json::from_value(v.clone()).ok()?;
    first_non_empty_list(doc.allowed_models, None, None)
}

/// Strict pick-one-of-three: first **non-empty** `Some(list)` where `list` is
/// non-empty after trim.
#[must_use]
pub fn first_non_empty_list(
    a: Option<Vec<String>>,
    b: Option<Vec<String>>,
    c: Option<Vec<String>>,
) -> Option<Vec<String>> {
    for list in [a, b, c].into_iter().flatten() {
        let v: Vec<String> = list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// `policy_overrides.overrides`：`"modelWhitelist": { "models": [ ... ] }`。
#[must_use]
pub fn model_ids_from_policy_overrides_json(v: &Value) -> Option<Vec<String>> {
    for ptr in ["/modelWhitelist/models", "/model_whitelist/models"] {
        if let Some(node) = v.pointer(ptr)
            && let Some(list) = non_empty_string_ids(node.clone())
        {
            return Some(list);
        }
    }
    if let Some(whitelist) = v.get("modelWhitelist").or_else(|| v.get("model_whitelist"))
        && let Some(m) = whitelist.get("models")
        && let Some(list) = non_empty_string_ids(m.clone())
    {
        return Some(list);
    }
    if let Some(ids) = model_ids_from_config_json(v) {
        return Some(ids);
    }
    v.get("model_access").and_then(model_ids_from_config_json)
}

/// Legacy alias: same as [`model_ids_from_policy_overrides_json`].
#[must_use]
pub fn model_ids_from_overrides_json(v: &Value) -> Option<Vec<String>> {
    model_ids_from_policy_overrides_json(v)
}

/// Among `(gateway_model, total price)` pairs pick the most expensive; ties
/// break by max **Unicode string order**. `scored` must be non-empty or this
/// panics (caller must keep ≥1 after filtering).
#[must_use]
pub fn pick_greatest_by_price_and_name(scored: &[(String, f64)]) -> &str {
    assert!(!scored.is_empty());
    let max = scored
        .iter()
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    let nearly = |a: f64, b: f64| (a - b).abs() <= 1e-9_f64.max(1e-9 * b.abs());
    scored
        .iter()
        .filter(|(_, s)| nearly(*s, max))
        .map(|(m, _)| m.as_str())
        .max()
        .expect("at least one score near max")
}

#[cfg(test)]
mod local_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn first_non_empty_respects_order() {
        let r = first_non_empty_list(None, Some(vec!["b".into()]), Some(vec!["a".into()]));
        assert_eq!(r, Some(vec!["b".to_string()]));
    }

    #[test]
    fn pick_tie_unicode() {
        // same price, pick lexicographic max
        let scored = vec![
            ("openai/aaa".to_string(), 1.0),
            ("zoo/bbb".to_string(), 1.0),
        ];
        assert_eq!(pick_greatest_by_price_and_name(&scored), "zoo/bbb");
    }

    #[test]
    fn pick_higher_price() {
        let scored = vec![("a/x".to_string(), 0.0), ("a/y".to_string(), 9.0)];
        assert_eq!(pick_greatest_by_price_and_name(&scored), "a/y");
    }

    #[test]
    fn config_allowed_models_camel() {
        let v = json!({ "allowedModels": ["gpt-4o", "  ", "b"] });
        let ids = model_ids_from_config_json(&v).unwrap();
        assert_eq!(ids, vec!["gpt-4o", "b"]);
    }

    #[test]
    fn overrides_model_whitelist() {
        let v = json!({
            "modelWhitelist": { "models": ["gpt-5.4-nano"] }
        });
        let ids = model_ids_from_policy_overrides_json(&v).unwrap();
        assert_eq!(ids, vec!["gpt-5.4-nano"]);
    }
}
