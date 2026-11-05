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
fn parse_disposition(text: &str) -> Result<std::collections::HashMap<String, String>, ApiError> {
    let malformed = || {
        invalid(
            StatusCode::BAD_REQUEST,
            "invalid multipart disposition parameters",
        )
    };
    let (kind, tail) = text.split_once(';').ok_or_else(malformed)?;
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(malformed());
    }
    let bytes = tail.as_bytes();
    let mut i = 0;
    let mut parameters = std::collections::HashMap::new();
    while i < bytes.len() {
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b' ' | b'\t' | b';') {
            i += 1;
        }
        let name = http::HeaderName::from_bytes(&bytes[start..i])
            .map_err(|_| malformed())?
            .to_string();
        if parameters.contains_key(&name) {
            return Err(malformed());
        }
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            return Err(malformed());
        }
        i += 1;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let value = if bytes.get(i) == Some(&b'"') {
            i += 1;
            let mut value = Vec::new();
            let mut closed = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i += 1;
                        closed = true;
                        break;
                    }
                    b'\\' => {
                        i += 1;
                        value.push(*bytes.get(i).ok_or_else(malformed)?);
                        i += 1;
                    }
                    byte => {
                        value.push(byte);
                        i += 1;
                    }
                }
            }
            if !closed {
                return Err(malformed());
            }
            String::from_utf8(value).map_err(|_| malformed())?
        } else {
            let start = i;
            while i < bytes.len() && !matches!(bytes[i], b';' | b' ' | b'\t') {
                i += 1;
            }
            http::HeaderName::from_bytes(&bytes[start..i]).map_err(|_| malformed())?;
            std::str::from_utf8(&bytes[start..i])
                .map_err(|_| malformed())?
                .to_owned()
        };
        http::HeaderValue::from_str(&value).map_err(|_| malformed())?;
        parameters.insert(name, value);
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if i < bytes.len() {
            if bytes[i] != b';' {
                return Err(malformed());
            }
            i += 1;
            if i == bytes.len() {
                return Err(malformed());
            }
        }
    }
    Ok(parameters)
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
        let mut parameters = HashSet::new();
        for (name, _) in mime.params() {
            if !parameters.insert(name.to_string().to_ascii_lowercase()) {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "repeated multipart content-type parameter",
                ));
            }
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
                    disposition = Some(
                        std::str::from_utf8(value.as_bytes())
                            .map_err(|_| malformed())?
                            .to_owned(),
                    );
                }
            }
            let parameters = parse_disposition(&disposition.ok_or_else(malformed)?)?;
            let name = parameters
                .get("name")
                .filter(|v| !v.is_empty())
                .ok_or_else(malformed)?
                .clone();
            let filename = parameters.get("filename").cloned();
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

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn form(extra: &str, filename: &str) -> Bytes {
        Bytes::from(format!(
            "--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/whisper-1\r\n--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"{extra}\r\nContent-Type: audio/wav\r\n\r\nRIFF\0payload\r\n--b--\r\n"
        ))
    }
    #[test]
    fn final_review_duplicate_disposition_and_boundary_parameters_are_rejected() {
        for suffix in [
            "; name=\"model\"",
            "; NAME=\"mask\"",
            "; filename=\"other.wav\"",
        ] {
            assert!(
                Multipart::parse(form(suffix, "test.wav"), "multipart/form-data; boundary=b")
                    .is_err(),
                "ambiguous suffix {suffix}"
            );
        }
        assert!(
            Multipart::parse(
                form("", "test.wav"),
                "multipart/form-data; boundary=b; boundary=evil"
            )
            .is_err()
        );
        let ambiguous=Bytes::from_static(b"--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/whisper-1\r\n--b\r\nContent-Disposition: form-data; name=\"prompt\"; name=\"model\"\r\n\r\nblocked-model\r\n--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n\r\nRIFF\r\n--b--\r\n");
        assert!(Multipart::parse(ambiguous, "multipart/form-data; boundary=b").is_err());
    }
    #[test]
    fn final_review_utf8_and_escaped_filenames_preserve_upload_bytes() {
        for (wire, filename) in [
            ("录音.wav", "录音.wav"),
            ("照片.png", "照片.png"),
            ("a\\\"b; c.wav", "a\"b; c.wav"),
        ] {
            let raw = form("", wire);
            let parsed = Multipart::parse(raw.clone(), "multipart/form-data; boundary=b")
                .expect("valid UTF-8 filename");
            assert_eq!(parsed.parts[1].filename.as_deref(), Some(filename));
            let expected = Bytes::from(String::from_utf8(raw.to_vec()).unwrap().replacen(
                "openai/whisper-1",
                "whisper-1",
                1,
            ));
            assert_eq!(parsed.rewrite_model("whisper-1").unwrap(), expected);
        }
    }
}
