use crate::endpoints::Endpoint;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AudioSpeech;
impl Endpoint for AudioSpeech {
    const PATH: &'static str = "v1/audio/speech";
    type RequestBody = bytes::Bytes;
    type ResponseBody = bytes::Bytes;
    type StreamResponseBody = bytes::Bytes;
    type ErrorResponseBody = serde_json::Value;
}
