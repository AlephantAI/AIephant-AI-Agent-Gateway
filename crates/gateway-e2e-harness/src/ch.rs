//! ClickHouse HTTP client for `ch.steps`.

use std::time::Duration;

use anyhow::Context;
use reqwest::Url;

use crate::{
    captured::CapturedResponse,
    case::{ChPoll, ChSpec, ChStep},
};

#[derive(Debug, Clone)]
pub struct ChClient {
    pub client: reqwest::Client,
    pub query_url: Url,
    pub basic_auth: Option<(String, String)>,
}

impl ChClient {
    /// Build from `CLICKHOUSE_HTTP_URL` (e.g. `http://127.0.0.1:8123`) or `CLICKHOUSE_URL`,
    /// optional `CLICKHOUSE_DATABASE`, and optional `CLICKHOUSE_USER` /
    /// `CLICKHOUSE_PASSWORD`.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let base = match std::env::var("CLICKHOUSE_HTTP_URL")
            .or_else(|_| std::env::var("CLICKHOUSE_URL"))
        {
            Ok(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };
        let mut query_url = Url::parse(&base).context("CLICKHOUSE_HTTP_URL / CLICKHOUSE_URL")?;
        if let Ok(db) = std::env::var("CLICKHOUSE_DATABASE")
            && !db.is_empty()
        {
            query_url.query_pairs_mut().append_pair("database", &db);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let user = std::env::var("CLICKHOUSE_USER").unwrap_or_default();
        let pass = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default();
        let basic_auth = if user.is_empty() {
            None
        } else {
            Some((user, pass))
        };
        Ok(Some(Self {
            client,
            query_url,
            basic_auth,
        }))
    }

    /// Query `request_response_rmt` by `request_id`, returning the row as JSON
    /// (or `null` if no row found after polling).
    pub async fn lookup_request_row(
        &self,
        request_id: &str,
        poll: &ChPoll,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let sql = format!(
            "SELECT * FROM request_response_rmt WHERE request_id = \
             '{request_id}' FORMAT JSONEachRow"
        );
        for attempt in 0..poll.max_attempts {
            match self.run_sql(&sql).await {
                Ok(body) => {
                    let rows = parse_json_each_row_lines(&body);
                    if let Some(row) = rows.into_iter().next() {
                        return Ok(Some(row));
                    }
                    if attempt + 1 == poll.max_attempts {
                        return Ok(None);
                    }
                    tokio::time::sleep(Duration::from_millis(poll.backoff_ms)).await;
                }
                Err(e) => {
                    if attempt + 1 == poll.max_attempts {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(poll.backoff_ms)).await;
                }
            }
        }
        Ok(None)
    }

    pub async fn run_sql(&self, sql: &str) -> anyhow::Result<String> {
        let mut req = self
            .client
            .post(self.query_url.clone())
            .body(sql.to_string())
            .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8");
        if let Some((u, p)) = &self.basic_auth {
            req = req.basic_auth(u, Some(p));
        }
        let body = req
            .send()
            .await
            .context("clickhouse POST")?
            .error_for_status()
            .context("clickhouse HTTP error")?
            .text()
            .await
            .context("clickhouse read body")?;
        Ok(body)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChOutcome {
    pub last_rows: Vec<serde_json::Value>,
}

/// Replace `$ALEPHANT_ID` using the `alephant-id` response header (lowercase
/// map).
#[must_use]
pub fn expand_ch_sql(sql: &str, curl: &CapturedResponse) -> String {
    let id = curl
        .headers
        .get("alephant-id")
        .map(String::as_str)
        .unwrap_or("");
    sql.replace("$ALEPHANT_ID", id)
}

/// When `assert_poll` is `Some`, every `clickhouse_query` must be immediately
/// followed by `assert_row`, and every `assert_row` must be immediately
/// preceded by `clickhouse_query` (no `sleep` or second query in between).
fn validate_assert_poll_steps(
    steps: &[ChStep],
    assert_poll: Option<&ChPoll>,
) -> anyhow::Result<()> {
    let Some(_ap) = assert_poll else {
        return Ok(());
    };
    if steps.is_empty() {
        anyhow::bail!("ch.assertPoll: steps must not be empty when assertPoll is set");
    }
    let mut i = 0;
    while i < steps.len() {
        match &steps[i] {
            ChStep::ClickhouseQuery { .. } => {
                let next = steps.get(i + 1);
                if !matches!(next, Some(ChStep::AssertRow { .. })) {
                    anyhow::bail!(
                        "ch.assertPoll: each clickhouse_query must be \
                         immediately followed by assert_row; failed at step \
                         index {i}"
                    );
                }
                i += 2;
            }
            ChStep::AssertRow { .. } => {
                anyhow::bail!(
                    "ch.assertPoll: assert_row at index {i} is not preceded \
                     by clickhouse_query"
                );
            }
            ChStep::Sleep { .. } => {
                i += 1;
            }
        }
    }
    Ok(())
}

async fn run_sql_with_poll(ch: &ChClient, sql_exec: &str, poll: &ChPoll) -> anyhow::Result<String> {
    for attempt in 0..poll.max_attempts {
        match ch.run_sql(sql_exec).await {
            Ok(body) => return Ok(body),
            Err(e) => {
                if attempt + 1 == poll.max_attempts {
                    anyhow::bail!(
                        "ch: query: clickhouse query failed after {} \
                         attempts: {e:#}",
                        poll.max_attempts,
                    );
                }
                tokio::time::sleep(Duration::from_millis(poll.backoff_ms)).await;
            }
        }
    }
    anyhow::bail!("ch: query: internal poll exhausted")
}

fn assert_first_row_matches(
    last_rows: &[serde_json::Value],
    equals: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    let row = last_rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("AssertRow: no rows from previous clickhouseQuery"))?;
    let obj = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("AssertRow: first row is not a JSON object"))?;
    for (k, expected) in equals {
        let got = obj
            .get(k)
            .ok_or_else(|| anyhow::anyhow!("AssertRow: column {k:?} missing in row {obj:?}"))?;
        if got != expected {
            anyhow::bail!("AssertRow: column {k:?} expected {expected}, got {got}");
        }
    }
    Ok(())
}

#[must_use]
fn first_row_hint(rows: &[serde_json::Value]) -> String {
    let Some(first) = rows.first() else {
        return "<no rows>".to_string();
    };
    let s = first.to_string();
    const MAX: usize = 500;
    if s.len() > MAX {
        format!("{}…", &s[..MAX])
    } else {
        s
    }
}

pub async fn run_ch_spec(
    client: Option<&ChClient>,
    spec: &ChSpec,
    curl: Option<&CapturedResponse>,
) -> anyhow::Result<ChOutcome> {
    let Some(ch) = client else {
        anyhow::bail!(
            "case has `ch` steps but CLICKHOUSE_HTTP_URL (or CLICKHOUSE_URL) \
             is not set"
        );
    };

    validate_assert_poll_steps(&spec.steps, spec.assert_poll.as_ref())?;
    let poll = spec.poll.clone().unwrap_or_default();

    let mut out = ChOutcome::default();
    let mut i = 0usize;
    while i < spec.steps.len() {
        match &spec.steps[i] {
            ChStep::Sleep { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                i += 1;
            }
            ChStep::ClickhouseQuery { sql } => {
                if matches!(spec.steps.get(i + 1), Some(ChStep::AssertRow { .. })) {
                    let ChStep::AssertRow { equals } = &spec.steps[i + 1] else {
                        unreachable!("guarded by matches");
                    };
                    let sql_exec = curl
                        .map(|c| expand_ch_sql(sql, c))
                        .unwrap_or_else(|| sql.clone());
                    let body = run_sql_with_poll(ch, &sql_exec, &poll).await?;
                    out.last_rows = parse_json_each_row_lines(&body);

                    if let Some(ap) = spec.assert_poll.as_ref() {
                        for assert_attempt in 0..ap.max_attempts {
                            match assert_first_row_matches(&out.last_rows, equals) {
                                Ok(()) => break,
                                Err(e) => {
                                    if assert_attempt + 1 == ap.max_attempts {
                                        let hint = first_row_hint(&out.last_rows);
                                        anyhow::bail!(
                                            "ch: assertPoll: {e:#} (sample: \
                                             {hint})"
                                        );
                                    }
                                    tokio::time::sleep(Duration::from_millis(ap.backoff_ms)).await;
                                    let body = run_sql_with_poll(ch, &sql_exec, &poll).await?;
                                    out.last_rows = parse_json_each_row_lines(&body);
                                }
                            }
                        }
                    } else {
                        assert_first_row_matches(&out.last_rows, equals)
                            .map_err(|e| anyhow::anyhow!("ch: assert: {e:#}"))?;
                    }
                    i += 2;
                } else {
                    if spec.assert_poll.is_some() {
                        anyhow::bail!(
                            "ch.assertPoll: each clickhouse_query must be \
                             immediately followed by assert_row; failed at \
                             step index {i}"
                        );
                    }
                    let sql_exec = curl
                        .map(|c| expand_ch_sql(sql, c))
                        .unwrap_or_else(|| sql.clone());
                    let body = run_sql_with_poll(ch, &sql_exec, &poll).await?;
                    out.last_rows = parse_json_each_row_lines(&body);
                    i += 1;
                }
            }
            ChStep::AssertRow { equals } => {
                if spec.assert_poll.is_some() {
                    anyhow::bail!(
                        "ch.assertPoll: assert_row at index {i} is not \
                         preceded by clickhouse_query"
                    );
                }
                assert_first_row_matches(&out.last_rows, equals)
                    .map_err(|e| anyhow::anyhow!("ch: assert: {e:#}"))?;
                i += 1;
            }
        }
    }
    Ok(out)
}

fn parse_json_each_row_lines(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(t).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::captured::CapturedResponse;

    #[test]
    fn expand_ch_sql_replaces_id() {
        let mut h = HashMap::new();
        h.insert("alephant-id".into(), "rid-123".into());
        let cap = CapturedResponse {
            status: 200,
            headers: h,
            body: "{}".into(),
            time_total_sec: 0.0,
        };
        let sql = "SELECT 1 WHERE id = '$ALEPHANT_ID'";
        assert_eq!(expand_ch_sql(sql, &cap), "SELECT 1 WHERE id = 'rid-123'");
    }

    #[test]
    fn validate_assert_poll_rejects_query_sleep_assert() {
        use crate::case::{ChPoll, ChStep};
        let steps = vec![
            ChStep::ClickhouseQuery {
                sql: "SELECT 1".into(),
            },
            ChStep::Sleep { ms: 1 },
            ChStep::AssertRow {
                equals: Default::default(),
            },
        ];
        let ap = ChPoll::default();
        assert!(validate_assert_poll_steps(&steps, Some(&ap)).is_err());
    }

    #[test]
    fn validate_assert_poll_accepts_sleep_query_assert() {
        use crate::case::{ChPoll, ChStep};
        let steps = vec![
            ChStep::Sleep { ms: 1 },
            ChStep::ClickhouseQuery {
                sql: "SELECT 1".into(),
            },
            ChStep::AssertRow {
                equals: Default::default(),
            },
        ];
        let ap = ChPoll::default();
        assert!(validate_assert_poll_steps(&steps, Some(&ap)).is_ok());
    }

    #[test]
    fn assert_first_row_matches_ok() {
        let rows = vec![serde_json::json!({"k": 1})];
        let mut m = serde_json::Map::new();
        m.insert("k".into(), serde_json::json!(1));
        assert!(assert_first_row_matches(&rows, &m).is_ok());
    }

    #[test]
    fn assert_first_row_matches_fails_on_wrong_value() {
        let rows = vec![serde_json::json!({"k": 2})];
        let mut m = serde_json::Map::new();
        m.insert("k".into(), serde_json::json!(1));
        assert!(assert_first_row_matches(&rows, &m).is_err());
    }

    #[test]
    fn assert_first_row_matches_fails_on_empty_rows() {
        let rows: Vec<serde_json::Value> = vec![];
        let m = serde_json::Map::new();
        assert!(assert_first_row_matches(&rows, &m).is_err());
    }

    #[test]
    fn first_row_hint_truncates_long_json() {
        let rows = vec![serde_json::json!({"x": "y".repeat(300)})];
        assert!(first_row_hint(&rows).len() <= 502);
    }
}
