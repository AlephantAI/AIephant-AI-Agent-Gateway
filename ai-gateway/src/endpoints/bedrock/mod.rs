pub(crate) mod converse;

use super::EndpointType;
pub(crate) use crate::endpoints::bedrock::converse::Converse;
use crate::types::model_id::ModelId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter)]
pub enum Bedrock {
    Converse(Converse),
}

impl Bedrock {
    #[must_use]
    pub fn path(self, model_id: &ModelId, is_stream: bool) -> String {
        let model_id = bedrock_model_path_segment(model_id);
        match self {
            Self::Converse(_) => {
                if is_stream {
                    format!("model/{model_id}/converse-stream")
                } else {
                    format!("model/{model_id}/converse")
                }
            }
        }
    }

    #[must_use]
    pub fn converse() -> Self {
        Self::Converse(Converse)
    }

    #[must_use]
    pub fn endpoint_type(self) -> EndpointType {
        match self {
            Self::Converse(_) => EndpointType::Chat,
        }
    }
}

fn bedrock_model_path_segment(model_id: &ModelId) -> String {
    match model_id {
        ModelId::Unknown(raw) => percent_encode_path_segment(raw),
        _ => model_id.to_string(),
    }
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
        ) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        endpoints::bedrock::Bedrock,
        types::{
            model_id::{BedrockModelId, ModelId},
            provider::InferenceProvider,
        },
    };

    #[test]
    fn bedrock_unknown_model_path_segment_is_percent_encoded() {
        let model_id =
            ModelId::Unknown("anthropic/claude sonnet?x#y".to_string());

        let path = Bedrock::converse().path(&model_id, false);

        assert_eq!(path, "model/anthropic%2Fclaude%20sonnet%3Fx%23y/converse");
    }

    #[test]
    fn bedrock_known_model_path_segment_keeps_existing_display() {
        let model_id = ModelId::Bedrock(
            BedrockModelId::from_str("anthropic.claude-sonnet-4-v1:0")
                .expect("bedrock model should parse"),
        );

        let path = Bedrock::converse().path(&model_id, false);

        assert_eq!(path, "model/anthropic.claude-sonnet-4-v1:0/converse");
        assert_eq!(
            model_id.inference_provider(),
            Some(InferenceProvider::Bedrock)
        );
    }
}
