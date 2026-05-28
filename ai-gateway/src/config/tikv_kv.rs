use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// TiKV Raw KV for LLM response cache (`--features internal`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TikvKvConfig {
    /// PD endpoints (not TiKV nodes), e.g. `127.0.0.1:2379`. At least one when
    /// `tikv_kv` is present.
    #[serde(deserialize_with = "deserialize_pd_endpoints")]
    pub pd_endpoints: Vec<String>,
    #[serde(default)]
    pub ca_cert_path: Option<std::path::PathBuf>,
    /// Per-request gRPC timeout (ms). Default 2000; keep at or below bucket
    /// read budget.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_max_value_bytes")]
    pub max_value_bytes: usize,
}

fn default_request_timeout_ms() -> u64 {
    2000
}

fn default_max_value_bytes() -> usize {
    8 * 1024 * 1024
}

/// From `config` + env: `pd-endpoints` may be a YAML array, JSON-array string,
/// or `AI_GATEWAY__TIKV_KV__PD_ENDPOINTS__0`-generated `{"0":"..."}` map.
fn deserialize_pd_endpoints<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| D::Error::custom("pd_endpoints: array items must be strings"))
            })
            .collect(),
        serde_json::Value::String(s) => serde_json::from_str(&s).map_err(D::Error::custom),
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(usize, String)> = map
                .into_iter()
                .filter_map(|(k, v)| {
                    let idx = k.parse::<usize>().ok()?;
                    Some((idx, v.as_str()?.to_owned()))
                })
                .collect();
            pairs.sort_by_key(|(i, _)| *i);
            if pairs.is_empty() {
                return Err(D::Error::custom(
                    "pd_endpoints: expected non-empty indexed map",
                ));
            }
            Ok(pairs.into_iter().map(|(_, s)| s).collect())
        }
        _ => Err(D::Error::custom(
            "pd_endpoints: expected array, JSON-array string, or indexed map",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pd_endpoints_from_json_string() {
        let v: TikvKvConfig = serde_json::from_value(serde_json::json!({
            "pd-endpoints": "[\"127.0.0.1:2379\"]",
            "request-timeout-ms": 2000,
            "max-value-bytes": 100,
        }))
        .unwrap();
        assert_eq!(v.pd_endpoints, vec!["127.0.0.1:2379".to_owned()]);
    }

    #[test]
    fn pd_endpoints_from_indexed_map() {
        let v: TikvKvConfig = serde_json::from_value(serde_json::json!({
            "pd-endpoints": { "0": "127.0.0.1:2379" },
            "request-timeout-ms": 2000,
            "max-value-bytes": 100,
        }))
        .unwrap();
        assert_eq!(v.pd_endpoints, vec!["127.0.0.1:2379".to_owned()]);
    }
}
