use crate::endpoints::Endpoint;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ImageEdits;
impl Endpoint for ImageEdits {
    const PATH: &'static str = "v1/images/edits";
    type RequestBody = bytes::Bytes;
    type ResponseBody = bytes::Bytes;
    type StreamResponseBody = bytes::Bytes;
    type ErrorResponseBody = serde_json::Value;
}
