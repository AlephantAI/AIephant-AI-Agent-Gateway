use super::ValidationError;
use anyhow::{Context, Result, ensure};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct HeaderRule {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, deserialize_with = "enum_values")]
    pub allowed_values: Option<Vec<String>>,
    #[serde(default)]
    pub min_length: usize,
    #[serde(default = "default_max_length")]
    pub max_length: usize,
}

fn default_max_length() -> usize {
    1024
}

fn enum_values<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    Vec::<String>::deserialize(d).map(Some)
}

pub(super) fn business_header(name: &str) -> bool {
    if name.len() > 128 || HeaderName::from_bytes(name.as_bytes()).is_err() {
        return false;
    }
    // This supplier metadata header shares the prefix reserved for internal agent headers.
    if name.eq_ignore_ascii_case("x-agent-info") {
        return true;
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_bytes(name.as_bytes()).unwrap(),
        HeaderValue::from_static("header-check"),
    );
    crate::dispatcher::upstream_auth::UpstreamAuthApplier::sanitize_headers(&mut headers);
    headers.contains_key(name)
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "content-type"
                | "accept-encoding"
                | "cookie"
                | "set-cookie"
                | "keep-alive"
                | "proxy-connection"
                | "x-request-id"
                | "api-key"
                | "x-api-key"
                | "proxy-authorization"
                | "proxy-authenticate"
                | "host"
                | "connection"
                | "transfer-encoding"
                | "te"
                | "trailer"
                | "upgrade"
                | "content-length"
                | "content-encoding"
        )
        && !["x-forwarded-", "alephant-", "x-alephant-", "x-agent-"]
            .iter()
            .any(|p| name.to_ascii_lowercase().starts_with(p))
}

pub(super) fn check_value(
    headers: &HeaderMap,
    name: &str,
    required: bool,
    accepts: impl FnOnce(&[u8]) -> bool,
) -> Result<(), ValidationError> {
    let mut values = headers.get_all(name).iter();
    match values.next() {
        None if !required => Ok(()),
        Some(value) if values.next().is_none() && accepts(value.as_bytes()) => Ok(()),
        _ => Err(ValidationError::new("invalid_header", "/headers")),
    }
}

impl HeaderRule {
    pub fn normalize(&mut self) -> Result<()> {
        ensure!(business_header(&self.name), "invalid business header");
        self.name.make_ascii_lowercase();
        ensure!(
            self.min_length <= self.max_length && self.max_length <= 1024,
            "invalid header length bounds"
        );
        if let Some(values) = &mut self.allowed_values {
            ensure!((1..=128).contains(&values.len()), "invalid header enum");
            ensure!(
                values.iter().all(|v| v.len() <= 1024
                    && HeaderValue::from_str(v).is_ok()
                    && (self.min_length..=self.max_length).contains(&v.chars().count())),
                "invalid header enum"
            );
            values.sort();
            ensure!(
                values.windows(2).all(|w| w[0] != w[1]),
                "duplicate header enum"
            );
        }
        Ok(())
    }

    pub fn check(&self, headers: &HeaderMap) -> Result<(), ValidationError> {
        check_value(headers, &self.name, self.required, |value| {
            value.len() <= 1024
                && std::str::from_utf8(value).is_ok_and(|text| {
                    (self.min_length..=self.max_length).contains(&text.chars().count())
                })
                && self
                    .allowed_values
                    .as_ref()
                    .is_none_or(|values| values.iter().any(|allowed| value == allowed.as_bytes()))
        })
    }
}

// Only the header portion is executed here; body/query/path validation stays with its existing owner.
pub(super) fn compile_contract(value: &Value) -> Result<(Vec<HeaderRule>, HeaderMap)> {
    ensure!(
        serde_json::to_vec(value)?.len() <= 65536,
        "request contract too large"
    );
    ensure!(
        value["version"] == 1,
        "unsupported request contract version"
    );
    let inputs = value["inputs"]
        .as_array()
        .context("contract inputs missing")?;
    let mappings = value["mappings"]
        .as_array()
        .context("contract mappings missing")?;
    ensure!(
        inputs.len() <= 128 && mappings.len() <= 128,
        "request contract too large"
    );
    let mut rules = Vec::new();
    let mut names = BTreeSet::new();
    for input in inputs {
        let location = input["location"]
            .as_str()
            .context("input location missing")?;
        ensure!(
            matches!(location, "header" | "body" | "query" | "path"),
            "unsupported input location"
        );
        if location != "header" {
            continue;
        }
        ensure!(
            input.as_object().unwrap().keys().all(|key| matches!(
                key.as_str(),
                "location"
                    | "name"
                    | "type"
                    | "required"
                    | "nullable"
                    | "description"
                    | "enum_values"
                    | "min_length"
                    | "max_length"
            )),
            "unsupported header constraint"
        );
        ensure!(
            matches!(input["type"].as_str(), Some("string" | "boolean"))
                && input["nullable"] == false,
            "header must be a non-nullable string or boolean"
        );
        let allowed_values = if input["type"] == "boolean" {
            let values = match input.get("enum_values") {
                Some(values) => serde_json::from_value::<Vec<bool>>(values.clone())
                    .map_err(|_| anyhow::anyhow!("invalid boolean header enum"))?,
                None => vec![false, true],
            };
            Some(values.into_iter().map(|value| value.to_string()).collect())
        } else {
            input
                .get("enum_values")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?
        };
        let mut rule = HeaderRule {
            name: input["name"]
                .as_str()
                .context("header name missing")?
                .to_owned(),
            required: input["required"]
                .as_bool()
                .context("header required missing")?,
            allowed_values,
            min_length: input
                .get("min_length")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?
                .unwrap_or(0),
            max_length: input
                .get("max_length")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?
                .unwrap_or_else(default_max_length),
        };
        rule.normalize()?;
        ensure!(names.insert(rule.name.clone()), "duplicate header input");
        rules.push(rule);
    }
    let mut mapped = BTreeSet::new();
    for mapping in mappings {
        let source = mapping["source"]
            .as_str()
            .context("mapping source missing")?;
        let target = mapping["target"]
            .as_str()
            .context("mapping target missing")?;
        if source.starts_with("header:") || target.starts_with("header:") {
            ensure!(
                mapping
                    .as_object()
                    .unwrap()
                    .keys()
                    .all(|k| matches!(k.as_str(), "source" | "target")),
                "unsupported header mapping"
            );
            let name = source
                .strip_prefix("header:")
                .context("header identity mapping required")?
                .to_ascii_lowercase();
            ensure!(
                target
                    .strip_prefix("header:")
                    .is_some_and(|t| t.eq_ignore_ascii_case(&name))
                    && names.contains(&name)
                    && mapped.insert(name),
                "header identity mapping required"
            );
        }
    }
    ensure!(mapped == names, "header mapping missing");
    let mut fixed_headers = HeaderMap::new();
    if let Some(fixed) = value.get("fixed_headers") {
        let fixed = fixed.as_object().context("invalid fixed headers")?;
        ensure!(fixed.len() <= 128, "too many fixed headers");
        for (name, value) in fixed {
            ensure!(business_header(name), "invalid fixed header");
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| anyhow::anyhow!("invalid fixed header"))?;
            ensure!(
                !names.contains(name.as_str()) && !fixed_headers.contains_key(&name),
                "fixed header conflicts with header input"
            );
            let value = value.as_str().context("invalid fixed header value")?;
            ensure!(value.len() <= 1024, "invalid fixed header value");
            let value = HeaderValue::from_str(value)
                .map_err(|_| anyhow::anyhow!("invalid fixed header value"))?;
            fixed_headers.insert(name, value);
        }
    }
    Ok((rules, fixed_headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::ValidationPolicy;
    use serde_json::json;
    fn contract() -> Value {
        json!({"version":1,"inputs":[{"location":"header","name":"Experiment-Mode","type":"boolean","required":true,"nullable":false}],"mappings":[{"source":"header:Experiment-Mode","target":"header:experiment-mode"}],"fixed_headers":{"X-Product":"gateway"}})
    }
    #[test]
    fn strict_boolean_required_duplicate_length_and_reserved_header_boundaries() {
        let policy = ValidationPolicy::for_route(None, Some(&contract()))
            .unwrap()
            .unwrap();
        let mut headers = HeaderMap::new();
        assert!(policy.validate_headers(&headers).is_err());
        for wire in ["TRUE", "1", "yes", ""] {
            headers.insert("experiment-mode", wire.parse().unwrap());
            assert!(policy.validate_headers(&headers).is_err());
        }
        for wire in ["true", "false"] {
            headers.insert("experiment-mode", wire.parse().unwrap());
            policy.validate_headers(&headers).unwrap();
        }
        headers.append("experiment-mode", "true".parse().unwrap());
        assert!(policy.validate_headers(&headers).is_err());
        for name in [
            "authorization",
            "api-key",
            "x-api-key",
            "host",
            "cookie",
            "connection",
            "transfer-encoding",
            "content-type",
            "x-forwarded-for",
            "alephant-api-key",
            "alephant-run-id",
            "x-alephant-secret",
        ] {
            let mut c = contract();
            c["fixed_headers"] = json!({name:"secret"});
            assert!(
                ValidationPolicy::for_route(None, Some(&c)).is_err(),
                "{name}"
            );
        }
        let mut conflict = contract();
        conflict["fixed_headers"] = json!({"Experiment-Mode":"true"});
        assert!(ValidationPolicy::for_route(None, Some(&conflict)).is_err());
        let mut bad = contract();
        bad["inputs"][0]["required"] = json!("false");
        assert!(ValidationPolicy::for_route(None, Some(&bad)).is_err());
        let p=ValidationPolicy::parse(&json!({"version":2,"request_checks":[{"kind":"header","name":"x-label","min_length":2,"max_length":3}]})).unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-label", "x".parse().unwrap());
        assert!(p.validate_headers(&h).is_err());
        h.insert("x-label", "xyz".parse().unwrap());
        p.validate_headers(&h).unwrap();
    }
}
