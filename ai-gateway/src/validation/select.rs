use super::ValidationError;
use serde::Deserialize;
use serde_json::Value;

pub(super) const MAX_ITEMS: usize = 1024;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Each {
    #[default]
    Single,
    ObjectValue,
    ArrayItem,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Filter {
    pub pointer: String,
    pub equals: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selector {
    pub pointer: String,
    #[serde(default)]
    pub each: Each,
    #[serde(default = "yes")]
    pub required: bool,
    #[serde(default)]
    pub min_items: usize,
    #[serde(default, rename = "where")]
    pub filter: Option<Filter>,
    #[serde(default)]
    pub value_pointer: String,
}

pub(super) const fn yes() -> bool {
    true
}

pub(super) fn valid_pointer(pointer: &str) -> bool {
    if pointer.len() > 512 || (!pointer.is_empty() && !pointer.starts_with('/')) {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

pub(super) struct Selected<'a> {
    pub key: String,
    pub value: &'a Value,
}

impl Selector {
    pub fn validate(&self) -> bool {
        valid_pointer(&self.pointer)
            && valid_pointer(&self.value_pointer)
            && self.min_items <= MAX_ITEMS
            && self.filter.as_ref().is_none_or(|f| {
                valid_pointer(&f.pointer)
                    && matches!(
                        f.equals,
                        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null
                    )
            })
    }

    pub fn select<'a>(&self, root: &'a Value) -> Result<Vec<Selected<'a>>, ValidationError> {
        let fail = |code| ValidationError::new(code, &self.pointer);
        let Some(collection) = root.pointer(&self.pointer) else {
            return if self.required || self.min_items > 0 {
                Err(fail("missing_field"))
            } else {
                Ok(vec![])
            };
        };
        let items: Vec<(String, &Value)> = match self.each {
            Each::Single => vec![(String::new(), collection)],
            Each::ObjectValue => {
                let object = collection
                    .as_object()
                    .ok_or_else(|| fail("expected_object"))?;
                if object.len() > MAX_ITEMS {
                    return Err(fail("too_many_items"));
                }
                if object.keys().map(String::len).sum::<usize>() > 65536 {
                    return Err(fail("selection_keys_too_large"));
                }
                object.iter().map(|(k, v)| (k.clone(), v)).collect()
            }
            Each::ArrayItem => {
                let array = collection
                    .as_array()
                    .ok_or_else(|| fail("expected_array"))?;
                if array.len() > MAX_ITEMS {
                    return Err(fail("too_many_items"));
                }
                array
                    .iter()
                    .enumerate()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect()
            }
        };
        let mut result = Vec::new();
        for (key, value) in items {
            if let Some(filter) = &self.filter {
                if !value.is_object() && !value.is_array() {
                    return Err(fail("invalid_item"));
                }
                let field = value
                    .pointer(&filter.pointer)
                    .ok_or_else(|| fail("missing_discriminator"))?;
                if field != &filter.equals {
                    continue;
                }
            }
            match value.pointer(&self.value_pointer) {
                Some(value) => result.push(Selected { key, value }),
                None if self.required => return Err(fail("missing_field")),
                None => {}
            }
        }
        if result.len() < self.min_items {
            return Err(fail("too_few_items"));
        }
        Ok(result)
    }
}
