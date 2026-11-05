use super::{
    HeaderRequirement, MAX_BODY_BYTES, ValidationError, WorkBudget,
    select::{Each, Selector, valid_pointer},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::Value;
use std::io::Cursor;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dimension {
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Format {
    Png,
    #[serde(alias = "jpg")]
    Jpeg,
}
impl Format {
    fn format(&self) -> &str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct References {
    pub search: Vec<Selector>,
    pub max_key_bytes: usize,
    pub max_total_key_bytes: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageRule {
    pub select: Selector,
    pub data_pointer: String,
    pub encoding: String,
    pub formats: Vec<Format>,
    pub max_decoded_bytes: usize,
    pub width: Dimension,
    pub height: Dimension,
    #[serde(default)]
    pub format_pointer: Option<String>,
    #[serde(default)]
    pub allow_animation: bool,
    #[serde(default)]
    pub forbidden_fields: Vec<String>,
    #[serde(default)]
    pub required_header: Option<HeaderRequirement>,
    #[serde(default)]
    pub references: Option<References>,
}

impl ImageRule {
    pub fn valid(&self) -> bool {
        let dimension = |d: &Dimension| d.min > 0 && d.min <= d.max && d.max <= 8192;
        self.select.validate()
            && valid_pointer(&self.data_pointer)
            && self.encoding == "base64"
            && !self.formats.is_empty()
            && self.formats.len() <= 3
            && (1..=MAX_BODY_BYTES).contains(&self.max_decoded_bytes)
            && dimension(&self.width)
            && dimension(&self.height)
            && self
                .format_pointer
                .as_ref()
                .is_none_or(|p| valid_pointer(p))
            && self.forbidden_fields.len() <= 16
            && self.forbidden_fields.iter().all(|p| valid_pointer(p))
            && self
                .required_header
                .as_ref()
                .is_none_or(HeaderRequirement::valid)
            && self.references.as_ref().is_none_or(|r| {
                self.select.each == Each::ObjectValue
                    && !r.search.is_empty()
                    && r.search.len() <= 16
                    && r.search.iter().all(Selector::validate)
                    && r.max_key_bytes > 0
                    && r.max_key_bytes <= r.max_total_key_bytes
                    && r.max_total_key_bytes <= 65536
            })
    }

    pub fn check(
        &self,
        root: &Value,
        headers: &http::HeaderMap,
        budget: &mut WorkBudget,
    ) -> Result<usize, ValidationError> {
        let items = self.select.select(root)?;
        let fail = |code| ValidationError::new(code, &self.select.pointer);
        if let Some(header) = &self.required_header {
            header.check(headers, !items.is_empty())?;
        }
        let mut key_bytes = 0usize;
        for item in &items {
            if self
                .forbidden_fields
                .iter()
                .any(|p| item.value.pointer(p).is_some())
            {
                return Err(fail("forbidden_field"));
            }
            if let Some(references) = &self.references {
                key_bytes += item.key.len();
                if item.key.trim().is_empty()
                    || item.key.len() > references.max_key_bytes
                    || key_bytes > references.max_total_key_bytes
                {
                    return Err(fail("invalid_reference"));
                }
                let mut found = false;
                for search in &references.search {
                    for value in search.select(root)? {
                        found |= referenced(
                            value.value,
                            &item.key,
                            &mut budget.reference_bytes,
                            &mut budget.reference_nodes,
                        )
                        .map_err(|()| fail("reference_work_limit"))?;
                        if found {
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                if !found {
                    return Err(fail("unused_reference"));
                }
            }
            let text = item
                .value
                .pointer(&self.data_pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| fail("missing_image_data"))?;
            if text.len() > self.max_decoded_bytes.div_ceil(3) * 4 {
                return Err(fail("image_too_large"));
            }
            let bytes = STANDARD.decode(text).map_err(|_| fail("invalid_base64"))?;
            if bytes.len() > self.max_decoded_bytes {
                return Err(fail("image_too_large"));
            }
            let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png"
            } else if bytes.starts_with(&[0xff, 0xd8]) {
                "jpeg"
            } else {
                return Err(fail("unsupported_image_format"));
            };
            if !self.formats.iter().any(|f| f.format() == format) {
                return Err(fail("unsupported_image_format"));
            }
            if let Some(pointer) = &self.format_pointer {
                let declared = item
                    .value
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .ok_or_else(|| fail("missing_image_format"))?;
                let expected = match declared {
                    "png" => "png",
                    "jpg" | "jpeg" => "jpeg",
                    _ => return Err(fail("unsupported_image_format")),
                };
                if format != expected {
                    return Err(fail("image_format_mismatch"));
                }
            }
            if format == "png" {
                let mut decoder = png::Decoder::new(Cursor::new(&bytes));
                decoder.set_limits(png::Limits {
                    bytes: 64 * 1024 * 1024,
                });
                decoder.set_ignore_text_chunk(true);
                decoder.set_ignore_iccp_chunk(true);
                let info = decoder
                    .read_header_info()
                    .map_err(|_| fail("invalid_image"))?;
                self.dimensions(info.width, info.height)?;
                let mut reader = decoder.read_info().map_err(|_| fail("invalid_image"))?;
                let info = reader.info();
                let frames = if let Some(animation) = info.animation_control {
                    if !self.allow_animation {
                        return Err(fail("animated_image"));
                    }
                    animation
                        .num_frames
                        .checked_add(u32::from(info.frame_control.is_none()))
                        .ok_or_else(|| fail("image_work_limit"))?
                } else {
                    1
                };
                if frames == 0
                    || frames > 32
                    || u64::from(info.width) * u64::from(info.height) * u64::from(frames)
                        > 16 * 1024 * 1024
                {
                    return Err(fail("image_work_limit"));
                }
                let size = reader
                    .output_buffer_size()
                    .filter(|size| *size <= 64 * 1024 * 1024)
                    .ok_or_else(|| fail("image_work_limit"))?;
                budget.image_bytes = budget
                    .image_bytes
                    .checked_sub(size as u64 * u64::from(frames))
                    .ok_or_else(|| fail("image_work_limit"))?;
                let mut buffer = vec![0; size];
                for _ in 0..frames {
                    reader
                        .next_frame(&mut buffer)
                        .map_err(|_| fail("invalid_image"))?;
                }
                // next_frame alone accepts truncated files with no IEND; finish checks the remaining chunks.
                reader.finish().map_err(|_| fail("invalid_image"))?;
            } else {
                if !bytes.ends_with(&[0xff, 0xd9]) {
                    return Err(fail("invalid_image"));
                }
                let options = zune_core::options::DecoderOptions::default()
                    .set_strict_mode(true)
                    .set_max_width(self.width.max as usize)
                    .set_max_height(self.height.max as usize);
                let mut decoder = zune_jpeg::JpegDecoder::new_with_options(
                    zune_core::bytestream::ZCursor::new(bytes.as_slice()),
                    options,
                );
                decoder
                    .decode_headers()
                    .map_err(|_| fail("invalid_image"))?;
                let (w, h) = decoder.dimensions().ok_or_else(|| fail("invalid_image"))?;
                self.dimensions(w as u32, h as u32)?;
                let size = decoder
                    .output_buffer_size()
                    .filter(|size| *size <= 64 * 1024 * 1024)
                    .ok_or_else(|| fail("image_work_limit"))?;
                budget.image_bytes = budget
                    .image_bytes
                    .checked_sub(size as u64)
                    .ok_or_else(|| fail("image_work_limit"))?;
                decoder
                    .decode_into(&mut vec![0; size])
                    .map_err(|_| fail("invalid_image"))?;
            }
        }
        Ok(items.len())
    }

    fn dimensions(&self, w: u32, h: u32) -> Result<(), ValidationError> {
        if w < self.width.min || h < self.height.min || w > self.width.max || h > self.height.max {
            return Err(ValidationError::new(
                "image_dimensions",
                &self.select.pointer,
            ));
        }
        Ok(())
    }
}

fn referenced(value: &Value, key: &str, budget: &mut usize, nodes: &mut usize) -> Result<bool, ()> {
    *nodes = nodes.checked_sub(1).ok_or(())?;
    match value {
        Value::String(s) => {
            *budget = budget.checked_sub(s.len()).ok_or(())?;
            Ok(s.contains(key))
        }
        Value::Array(items) => {
            for item in items {
                if referenced(item, key, budget, nodes)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Object(items) => {
            for item in items.values() {
                if referenced(item, key, budget, nodes)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_scan_budgets_nodes_without_string_values() {
        let value = serde_json::json!([null, 0, null, 0, null, 0, null, 0, null, 0]);
        assert!(
            referenced(&value, "absent", &mut 128, &mut 4).is_err(),
            "non-string nodes must not permit unlimited reference work"
        );
    }
}
