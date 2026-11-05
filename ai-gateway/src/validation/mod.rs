//! Adapted from openmodels-gateway (Apache-2.0), source baseline 42e793a.
//! Versioned media checks for Alephant model and tool targets. Checks never rewrite payloads.
mod header;
mod image;
mod select;
mod strict_json;

use self::{
    image::ImageRule,
    select::{Each, Selector, valid_pointer},
};
use anyhow::{Result, ensure};
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::{Semaphore, SemaphorePermit};

pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
static WORKERS: Semaphore = Semaphore::const_new(4);
#[cfg(test)]
pub(crate) static TEST_MEDIA_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, thiserror::Error)]
#[error("{code} at {path}")]
pub struct ValidationError {
    pub code: &'static str,
    pub path: String,
}
impl ValidationError {
    fn new(code: &'static str, path: &str) -> Self {
        Self {
            code,
            path: path.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeaderRequirement {
    name: String,
    value: String,
}
impl HeaderRequirement {
    fn valid(&self) -> bool {
        header::business_header(&self.name)
            && self.value.len() <= 1024
            && HeaderValue::from_str(&self.value).is_ok()
    }
    fn check(&self, headers: &HeaderMap, required: bool) -> Result<(), ValidationError> {
        header::check_value(headers, &self.name, required, |v| {
            v == self.value.as_bytes()
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Coordinates {
    left: String,
    top: String,
    right: String,
    bottom: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Representation {
    #[default]
    Corners,
    OriginSize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confidence {
    pointer: String,
    #[serde(default = "select::yes")]
    required: bool,
    #[serde(default)]
    nullable: bool,
    #[serde(default)]
    min: f64,
    #[serde(default = "one")]
    max: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoxRule {
    select: Selector,
    fields: Coordinates,
    min: f64,
    max: f64,
    #[serde(default)]
    representation: Representation,
    #[serde(default)]
    confidence: Option<Confidence>,
    #[serde(default)]
    request_selector: Option<Selector>,
    #[serde(default)]
    requires_image: bool,
    #[serde(default)]
    required_header: Option<HeaderRequirement>,
}
impl BoxRule {
    fn valid(&self) -> bool {
        let fields = [
            &self.fields.left,
            &self.fields.top,
            &self.fields.right,
            &self.fields.bottom,
        ];
        self.select.validate()
            && fields.iter().all(|p| valid_pointer(p))
            && fields.into_iter().collect::<BTreeSet<_>>().len() == 4
            && self.min.is_finite()
            && self.max.is_finite()
            && self.min < self.max
            && self.confidence.as_ref().is_none_or(|c| {
                valid_pointer(&c.pointer) && c.min.is_finite() && c.max.is_finite() && c.min < c.max
            })
            && self
                .required_header
                .as_ref()
                .is_none_or(HeaderRequirement::valid)
            && self.request_selector.as_ref().is_none_or(|s| {
                s.validate() && s.each != Each::Single && self.select.each == s.each
            })
    }
    fn check(
        &self,
        root: &Value,
        expected: Option<&BTreeSet<String>>,
    ) -> Result<usize, ValidationError> {
        let fail = |code| ValidationError::new(code, &self.select.pointer);
        let items = self.select.select(root)?;
        if let Some(expected) = expected {
            if items
                .iter()
                .map(|item| item.key.clone())
                .collect::<BTreeSet<_>>()
                != *expected
            {
                return Err(fail("response_items_mismatch"));
            }
        }
        for item in &items {
            let number = |pointer: &str| {
                item.value
                    .pointer(pointer)
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| fail("invalid_box_coordinate"))
            };
            let left = number(&self.fields.left)?;
            let top = number(&self.fields.top)?;
            let mut right = number(&self.fields.right)?;
            let mut bottom = number(&self.fields.bottom)?;
            if matches!(self.representation, Representation::OriginSize) {
                if right <= 0.0 || bottom <= 0.0 {
                    return Err(fail("invalid_box_order"));
                }
                right += left;
                bottom += top;
            }
            if ![left, top, right, bottom]
                .iter()
                .all(|v| v.is_finite() && *v >= self.min && *v <= self.max)
            {
                return Err(fail("box_out_of_range"));
            }
            if left >= right || top >= bottom {
                return Err(fail("invalid_box_order"));
            }
            if let Some(confidence) = &self.confidence {
                match item.value.pointer(&confidence.pointer) {
                    None if !confidence.required => {}
                    Some(Value::Null) if confidence.nullable => {}
                    Some(value)
                        if value.as_f64().is_some_and(|v| {
                            v.is_finite() && (confidence.min..=confidence.max).contains(&v)
                        }) => {}
                    _ => return Err(fail("invalid_box_confidence")),
                }
            }
        }
        Ok(items.len())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Check {
    Image(ImageRule),
    BoundingBox(BoxRule),
    Header(header::HeaderRule),
}
impl Check {
    fn header(&self) -> Option<&HeaderRequirement> {
        match self {
            Self::Image(rule) => rule.required_header.as_ref(),
            Self::BoundingBox(rule) => rule.required_header.as_ref(),
            Self::Header(_) => None,
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Image(rule) => rule.valid(),
            Self::BoundingBox(rule) => rule.valid(),
            Self::Header(_) => true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationPolicy {
    version: u8,
    #[serde(default)]
    request_checks: Vec<Check>,
    #[serde(default)]
    response_checks: Vec<Check>,
    #[serde(default)]
    denied_header_prefixes: Vec<String>,
    #[serde(skip)]
    fixed_headers: HeaderMap,
}

#[derive(Default)]
pub struct ValidationContext {
    expected: Vec<Option<BTreeSet<String>>>,
}

struct WorkBudget {
    image_bytes: u64,
    reference_bytes: usize,
    reference_nodes: usize,
}
impl Default for WorkBudget {
    fn default() -> Self {
        Self {
            image_bytes: 64 * 1024 * 1024,
            reference_bytes: 128 * 1024 * 1024,
            reference_nodes: 1_000_000,
        }
    }
}

impl ValidationPolicy {
    pub fn parse(value: &Value) -> Result<Self> {
        ensure!(
            serde_json::to_vec(value)?.len() <= 65536,
            "validation config exceeds size limit"
        );
        let mut policy: Self = serde_json::from_value(value.clone())
            .map_err(|_| anyhow::anyhow!("invalid validation config"))?;
        ensure!(
            matches!(policy.version, 1 | 2),
            "unsupported validation version"
        );
        let mut names = BTreeSet::new();
        for rule in &mut policy.request_checks {
            if let Check::Header(rule) = rule {
                ensure!(policy.version == 2, "header checks require validation v2");
                rule.normalize()?;
                ensure!(names.insert(rule.name.clone()), "duplicate header check");
            }
        }
        ensure!(
            policy.request_checks.len() + policy.response_checks.len() <= 32,
            "too many validation checks"
        );
        ensure!(
            policy
                .request_checks
                .iter()
                .chain(&policy.response_checks)
                .all(Check::valid),
            "invalid validation rule"
        );
        ensure!(
            policy
                .request_checks
                .iter()
                .all(|r| !matches!(r, Check::BoundingBox(b) if b.request_selector.is_some())),
            "request selector is only supported on response checks"
        );
        ensure!(
            policy.response_checks.iter().all(|r| match r {
                Check::BoundingBox(b) =>
                    (!b.requires_image && b.required_header.is_none())
                        || b.request_selector.is_some(),
                Check::Image(i) => i.required_header.is_none(),
                Check::Header(_) => false,
            }),
            "response dependency requires a request selector"
        );
        ensure!(
            policy.denied_header_prefixes.len() <= 16
                && policy.denied_header_prefixes.iter().all(|s| !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')),
            "invalid header prefix"
        );
        policy.validate_header_dependencies()?;
        Ok(policy)
    }

    pub fn for_route(validation: Option<&Value>, contract: Option<&Value>) -> Result<Option<Self>> {
        let mut policy = validation.map(Self::parse).transpose()?;
        if let Some(contract) = contract {
            let (rules, fixed_headers) = header::compile_contract(contract)?;
            if let Some(policy) = &policy {
                for check in &policy.request_checks {
                    if let Check::Header(header) = check {
                        ensure!(
                            rules.iter().any(|saved| saved == header),
                            "header validation exceeds saved contract"
                        );
                    }
                }
            }
            if !rules.is_empty() || !fixed_headers.is_empty() {
                let policy = policy.get_or_insert_with(|| Self {
                    version: 2,
                    request_checks: Vec::new(),
                    response_checks: Vec::new(),
                    denied_header_prefixes: Vec::new(),
                    fixed_headers: HeaderMap::new(),
                });
                policy.version = 2;
                policy.fixed_headers = fixed_headers;
                // Saved inputs are authoritative; a second declaration cannot change their meaning.
                for rule in rules {
                    if let Some(existing) = policy.request_checks.iter().find_map(|c| match c {
                        Check::Header(h) if h.name == rule.name => Some(h),
                        _ => None,
                    }) {
                        ensure!(
                            existing == &rule,
                            "header contract conflicts with validation"
                        );
                    } else {
                        policy.request_checks.push(Check::Header(rule));
                    }
                }
                ensure!(
                    policy.request_checks.len() + policy.response_checks.len() <= 32,
                    "too many validation checks"
                );
                policy.validate_header_dependencies()?;
            }
        }
        Ok(policy)
    }

    fn validate_header_dependencies(&self) -> Result<()> {
        let mut dependencies = std::collections::BTreeMap::new();
        for requirement in self
            .request_checks
            .iter()
            .chain(&self.response_checks)
            .filter_map(Check::header)
        {
            let name = requirement.name.to_ascii_lowercase();
            if let Some(value) = self.fixed_headers.get(&name) {
                ensure!(
                    value == requirement.value.as_str(),
                    "fixed header conflicts with validation"
                );
            }
            if let Some(previous) = dependencies.insert(name.clone(), &requirement.value) {
                ensure!(
                    previous == &requirement.value,
                    "conflicting header dependencies"
                );
            }
            for check in &self.request_checks {
                if let Check::Header(rule) = check {
                    if rule.name == name {
                        ensure!(
                            rule.allowed_values
                                .as_ref()
                                .is_some_and(|v| v.as_slice() == [requirement.value.clone()]),
                            "header dependency conflicts with declaration"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub fn has_response_checks(&self) -> bool {
        !self.response_checks.is_empty()
    }

    pub fn has_media_checks(&self) -> bool {
        self.request_checks
            .iter()
            .chain(&self.response_checks)
            .any(|c| !matches!(c, Check::Header(_)))
    }

    pub(crate) fn apply_upstream_headers(&self, incoming: &HeaderMap, outgoing: &mut HeaderMap) {
        let name = "x-agent-info";
        let declared = self
            .request_checks
            .iter()
            .chain(&self.response_checks)
            .any(|check| {
                matches!(check, Check::Header(rule) if rule.name == name)
                    || check
                        .header()
                        .is_some_and(|h| h.name.eq_ignore_ascii_case(name))
            });
        if declared {
            outgoing.remove(name);
            for value in incoming.get_all(name) {
                outgoing.append(name, value.clone());
            }
        }
        for (name, value) in &self.fixed_headers {
            outgoing.insert(name.clone(), value.clone());
        }
    }

    pub fn validate_headers(&self, headers: &HeaderMap) -> Result<(), ValidationError> {
        for rule in &self.request_checks {
            if let Check::Header(rule) = rule {
                rule.check(headers)?;
            }
        }
        for requirement in self
            .request_checks
            .iter()
            .chain(&self.response_checks)
            .filter_map(Check::header)
        {
            requirement.check(headers, false)?;
        }
        for name in headers.keys() {
            if self
                .denied_header_prefixes
                .iter()
                .any(|prefix| name.as_str().starts_with(prefix))
                && !self.fixed_headers.contains_key(name)
                && !self
                    .request_checks
                    .iter()
                    .any(|r| matches!(r, Check::Header(h) if h.name == name.as_str()))
                && !self
                    .request_checks
                    .iter()
                    .chain(&self.response_checks)
                    .filter_map(Check::header)
                    .any(|allowed| name.as_str().eq_ignore_ascii_case(&allowed.name))
            {
                return Err(ValidationError::new("forbidden_header", "/headers"));
            }
        }
        Ok(())
    }

    pub fn validate_request(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<ValidationContext, ValidationError> {
        self.validate_headers(headers)?;
        if !self.has_media_checks() {
            return Ok(ValidationContext::default());
        }
        let needs_request_json = self
            .request_checks
            .iter()
            .any(|r| !matches!(r, Check::Header(_)))
            || self
                .response_checks
                .iter()
                .any(|r| matches!(r, Check::BoundingBox(b) if b.request_selector.is_some()));
        if !needs_request_json {
            return Ok(ValidationContext {
                expected: vec![None; self.response_checks.len()],
            });
        }
        let root = parse(body)?;
        let images = self.check_images(&self.request_checks, &root, headers)?;
        for rule in &self.request_checks {
            if let Check::BoundingBox(rule) = rule {
                let count = rule.check(&root, None)?;
                check_dependencies(rule, count, images, headers)?;
            }
        }
        let mut context = ValidationContext::default();
        for rule in &self.response_checks {
            let expected = match rule {
                Check::BoundingBox(rule) => {
                    if let Some(selector) = &rule.request_selector {
                        let items = selector.select(&root)?;
                        check_dependencies(rule, items.len(), images, headers)?;
                        Some(items.into_iter().map(|item| item.key).collect())
                    } else {
                        None
                    }
                }
                Check::Image(_) | Check::Header(_) => None,
            };
            context.expected.push(expected);
        }
        Ok(context)
    }

    pub fn validate_response(
        &self,
        context: &ValidationContext,
        status: StatusCode,
        body: &[u8],
    ) -> Result<(), ValidationError> {
        if !status.is_success() || self.response_checks.is_empty() {
            return Ok(());
        }
        if context.expected.len() != self.response_checks.len() {
            return Err(ValidationError::new("missing_validation_context", "/"));
        }
        let root = parse(body)?;
        self.check_images(&self.response_checks, &root, &HeaderMap::new())?;
        for (index, rule) in self.response_checks.iter().enumerate() {
            if let Check::BoundingBox(rule) = rule {
                rule.check(&root, context.expected.get(index).and_then(Option::as_ref))?;
            }
        }
        Ok(())
    }

    fn check_images(
        &self,
        rules: &[Check],
        root: &Value,
        headers: &HeaderMap,
    ) -> Result<usize, ValidationError> {
        let mut total = 0usize;
        let mut budget = WorkBudget::default();
        for rule in rules {
            if let Check::Image(rule) = rule {
                total += rule.check(root, headers, &mut budget)?;
            }
        }
        Ok(total)
    }
}

fn check_dependencies(
    rule: &BoxRule,
    count: usize,
    images: usize,
    headers: &HeaderMap,
) -> Result<(), ValidationError> {
    if let Some(header) = &rule.required_header {
        header.check(headers, count > 0)?;
    }
    if count > 0 && rule.requires_image && images == 0 {
        return Err(ValidationError::new("image_required", &rule.select.pointer));
    }
    Ok(())
}

fn parse(body: &[u8]) -> Result<Value, ValidationError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(ValidationError::new("body_too_large", "/"));
    }
    strict_json::parse_json_strict(body).map_err(|_| ValidationError::new("invalid_json", "/"))
}

pub(crate) struct PreparedValidation {
    pub policy: Arc<ValidationPolicy>,
    pub context: Arc<ValidationContext>,
    // Hold admission through the upstream call, bounding buffered image requests too.
    permit: Option<Arc<SemaphorePermit<'static>>>,
}

pub(crate) fn admit() -> Result<SemaphorePermit<'static>, ValidationError> {
    WORKERS
        .try_acquire()
        .map_err(|_| ValidationError::new("validation_busy", "/"))
}

pub(crate) async fn prepare(
    policy: Arc<ValidationPolicy>,
    headers: HeaderMap,
    body: bytes::Bytes,
    permit: Option<SemaphorePermit<'static>>,
) -> Result<PreparedValidation, ValidationError> {
    if !policy.has_media_checks() {
        let context = policy.validate_request(&headers, &body)?;
        return Ok(PreparedValidation {
            policy,
            context: Arc::new(context),
            permit: None,
        });
    }
    tokio::task::spawn_blocking(move || {
        let context = policy.validate_request(&headers, &body)?;
        Ok(PreparedValidation {
            policy,
            context: Arc::new(context),
            permit: Some(Arc::new(permit.ok_or_else(|| {
                ValidationError::new("validation_unavailable", "/")
            })?)),
        })
    })
    .await
    .map_err(|_| ValidationError::new("validation_unavailable", "/"))?
}

pub(crate) async fn response(
    prepared: &PreparedValidation,
    status: StatusCode,
    body: bytes::Bytes,
) -> Result<(), ValidationError> {
    if !prepared.policy.has_media_checks() {
        return Ok(());
    }
    let policy = prepared.policy.clone();
    let context = prepared.context.clone();
    let permit = prepared.permit.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        policy.validate_response(&context, status, &body)
    })
    .await
    .map_err(|_| ValidationError::new("validation_unavailable", "/"))?
}

#[cfg(test)]
mod header_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn header_only_preparation_does_not_need_a_media_worker() {
        let _test_guard = TEST_MEDIA_LOCK.lock().await;
        let _permits: Vec<_> = (0..4).map(|_| admit().unwrap()).collect();
        assert!(admit().is_err());
        let policy = Arc::new(
            ValidationPolicy::parse(&json!({"version":2,"request_checks":[
                {"kind":"header","name":"Experiment-Mode","allowed_values":["true"]}
            ]}))
            .unwrap(),
        );
        assert!(!policy.has_media_checks());
        let prepared = prepare(policy, HeaderMap::new(), bytes::Bytes::new(), None)
            .await
            .unwrap();
        response(&prepared, StatusCode::OK, bytes::Bytes::new())
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod regression_tests;

mod integration;
pub(crate) use integration::{
    check_endpoint_response, prepare_endpoint, prepare_policy, validate_config,
};
