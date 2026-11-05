use crate::endpoints::Endpoint;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AudioTranscriptions;
impl Endpoint for AudioTranscriptions {
    const PATH: &'static str = "v1/audio/transcriptions";
    type RequestBody = bytes::Bytes;
    type ResponseBody = bytes::Bytes;
    type StreamResponseBody = bytes::Bytes;
    type ErrorResponseBody = serde_json::Value;
}
