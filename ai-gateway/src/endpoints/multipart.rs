//! Bounded, ordered multipart parsing. Only the model byte range is rewritten.
use crate::{endpoints::media::invalid, error::api::ApiError};
use bytes::{Bytes, BytesMut};
use http::StatusCode;
use std::{collections::HashSet, ops::Range};

#[derive(Debug)]
pub(crate) struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub body: Range<usize>,
}
pub(crate) struct Multipart {
    raw: Bytes,
    pub parts: Vec<Part>,
}
impl Multipart {
    pub fn parse(raw: Bytes, content_type: &str) -> Result<Self, ApiError> {
        let mime: mime::Mime = content_type.parse().map_err(|_| {
            invalid(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "invalid multipart content type",
            )
        })?;
        if mime.essence_str() != "multipart/form-data" {
            return Err(invalid(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "multipart/form-data required",
            ));
        }
        let boundary = mime
            .get_param("boundary")
            .map(|v| v.as_str())
            .filter(|b| {
                !b.is_empty() && b.len() <= 70 && b.bytes().all(|c| (0x21..=0x7e).contains(&c))
            })
            .ok_or_else(|| invalid(StatusCode::BAD_REQUEST, "invalid multipart boundary"))?;
        let marker = format!("--{boundary}");
        let separator = format!("\r\n--{boundary}");
        let malformed = || invalid(StatusCode::BAD_REQUEST, "malformed multipart body");
        let mut parts = Vec::new();
        let mut pos = 0;
        loop {
            if !raw
                .get(pos..)
                .is_some_and(|s| s.starts_with(marker.as_bytes()))
            {
                return Err(malformed());
            }
            pos += marker.len();
            if raw.get(pos..pos + 2) == Some(b"--") {
                if !matches!(raw.get(pos + 2..), Some(b"" | b"\r\n")) {
                    return Err(malformed());
                }
                break;
            }
            if raw.get(pos..pos + 2) != Some(b"\r\n") {
                return Err(malformed());
            }
            pos += 2;
            let end = raw[pos..]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|n| pos + n)
                .ok_or_else(malformed)?;
            if end - pos > 8192 || parts.len() >= 128 {
                return Err(invalid(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too many multipart parts or headers",
                ));
            }
            let text = std::str::from_utf8(&raw[pos..end]).map_err(|_| malformed())?;
            let mut names = HashSet::new();
            let mut disposition = None;
            for line in text.split("\r\n") {
                let (name, value) = line.split_once(':').ok_or_else(malformed)?;
                let name: http::HeaderName = name.parse().map_err(|_| malformed())?;
                let value = http::HeaderValue::from_str(value.trim()).map_err(|_| malformed())?;
                if !names.insert(name.clone()) {
                    return Err(malformed());
                }
                if name == http::header::CONTENT_DISPOSITION {
                    disposition = Some(value.to_str().map_err(|_| malformed())?.to_string());
                }
            }
            // mime's parameter parser handles quoted names and filenames without splitting on embedded semicolons.
            let disposition: mime::Mime =
                format!("application/{}", disposition.ok_or_else(malformed)?)
                    .parse()
                    .map_err(|_| malformed())?;
            if disposition.subtype() != "form-data" {
                return Err(malformed());
            }
            let name = disposition
                .get_param("name")
                .map(|v| v.as_str().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(malformed)?;
            let filename = disposition
                .get_param("filename")
                .map(|v| v.as_str().to_string());
            let start = end + 4;
            // A boundary-like byte sequence inside a file is data unless followed by CRLF or --.
            let end = raw[start..]
                .windows(separator.len())
                .enumerate()
                .find_map(|(i, w)| {
                    let next = start + i + separator.len();
                    (w == separator.as_bytes()
                        && matches!(raw.get(next..next + 2), Some(b"\r\n" | b"--")))
                    .then_some(start + i)
                })
                .ok_or_else(malformed)?;
            parts.push(Part {
                name,
                filename,
                body: start..end,
            });
            pos = end + 2;
        }
        Ok(Self { raw, parts })
    }
    pub fn unique_text(&self, name: &str) -> Result<&str, ApiError> {
        let found: Vec<_> = self.parts.iter().filter(|p| p.name == name).collect();
        match found.as_slice() {
            [part] if part.filename.is_none() => std::str::from_utf8(&self.raw[part.body.clone()])
                .ok()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| invalid(StatusCode::BAD_REQUEST, "invalid multipart text field")),
            _ => Err(invalid(
                StatusCode::BAD_REQUEST,
                "missing or repeated multipart field",
            )),
        }
    }
    pub fn validate_singletons(&self, repeated: &[&str]) -> Result<(), ApiError> {
        let mut seen = HashSet::new();
        for p in &self.parts {
            if !repeated.contains(&p.name.as_str()) && !seen.insert(&p.name) {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "repeated singleton multipart field",
                ));
            }
        }
        Ok(())
    }
    pub fn file_count(&self, name: &str) -> usize {
        self.parts
            .iter()
            .filter(|p| p.name == name && p.filename.is_some() && !p.body.is_empty())
            .count()
    }
    pub fn rewrite_model(&self, model: &str) -> Result<Bytes, ApiError> {
        self.unique_text("model")?;
        let part = self
            .parts
            .iter()
            .find(|p| p.name == "model")
            .expect("validated model");
        let mut out = BytesMut::with_capacity(self.raw.len() + model.len());
        out.extend_from_slice(&self.raw[..part.body.start]);
        out.extend_from_slice(model.as_bytes());
        out.extend_from_slice(&self.raw[part.body.end..]);
        Ok(out.freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multipart_rewrite_changes_only_model_even_with_boundary_like_file_bytes() {
        let raw=Bytes::from_static(b"--abc\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/whisper-1\r\n--abc\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a; test.wav\"\r\nContent-Type: audio/wav\r\n\r\n\xff\x00\r\n--abcXYZfile\r\n--abc--\r\n");
        let form = Multipart::parse(raw.clone(), "multipart/form-data; boundary=\"abc\"").unwrap();
        assert_eq!(form.parts[1].filename.as_deref(), Some("a; test.wav"));
        let out = form.rewrite_model("whisper-1").unwrap();
        let expected = raw
            .windows(b"openai/".len())
            .position(|w| w == b"openai/")
            .unwrap();
        let mut bytes = raw.to_vec();
        bytes.drain(expected..expected + 7);
        assert_eq!(out.as_ref(), bytes);
    }
    #[test]
    fn multipart_rejects_truncation_repeated_headers_and_ambiguous_model() {
        let field = "--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nx\r\n";
        let form = Multipart::parse(
            Bytes::from(format!("{field}{field}--b--\r\n")),
            "multipart/form-data; boundary=b",
        )
        .unwrap();
        assert!(form.unique_text("model").is_err());
        assert!(form.validate_singletons(&[]).is_err());
        assert!(Multipart::parse(Bytes::from(field), "multipart/form-data; boundary=b").is_err());
        assert!(Multipart::parse(Bytes::from_static(b"--b\r\nContent-Disposition: form-data; name=\"model\"\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--"),"multipart/form-data; boundary=b").is_err());
    }
}
