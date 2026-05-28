pub mod anthropic;
pub(crate) mod bedrock;
pub mod google;
pub mod mappings;
pub mod ollama;
pub mod openai;

use serde::{Deserialize, Serialize};

use crate::{
    endpoints::{
        anthropic::Anthropic, bedrock::Bedrock, google::Google, ollama::Ollama, openai::OpenAI,
    },
    error::{internal::InternalError, invalid_req::InvalidRequestError, mapper::MapperError},
    types::{model_id::ModelId, provider::InferenceProvider},
};

pub trait Endpoint {
    const PATH: &'static str;
    type RequestBody;
    type ResponseBody;
    type ErrorResponseBody;
    /// To support streaming response body types with different
    /// concrete type than the regular response body type.
    type StreamResponseBody;
}

macro_rules! define_endpoints {
    ($(($variant:ident, $path:literal)),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum EndpointRoute {
            $($variant,)*
        }

        impl EndpointRoute {
            #[must_use]
            pub const fn path(&self) -> &'static str {
                match self {
                    $(Self::$variant => $path,)*
                }
            }

            #[must_use]
            pub fn from_path(path: &str) -> Option<Self> {
                match path {
                    $($path => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

define_endpoints! {
    (ChatCompletions, "chat/completions"),
    (Completions, "completions"),
    (Embeddings, "embeddings"),
    (ImageGenerations, "images/generations"),
    (Responses, "responses"),
    (Messages, "messages"),
}

pub trait AiRequest {
    fn is_stream(&self) -> bool;
    fn model(&self) -> Result<ModelId, MapperError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ApiEndpoint {
    OpenAI(OpenAI),
    Anthropic(Anthropic),
    Google(Google),
    Ollama(Ollama),
    Bedrock(Bedrock),
    OpenAICompatible {
        provider: InferenceProvider,
        openai_endpoint: OpenAI,
    },
}

impl ApiEndpoint {
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        let r = EndpointRoute::from_path(path)?;
        if matches!(r, EndpointRoute::Messages) {
            return Some(Self::Anthropic(Anthropic::messages()));
        }
        OpenAI::try_from(&r).ok().map(Self::OpenAI)
    }

    pub fn mapped(
        source_endpoint: ApiEndpoint,
        target_provider: &InferenceProvider,
    ) -> Result<Self, InvalidRequestError> {
        const CROSS: &str = "this path is only routed to the same OpenAPI \
                             family (e.g. OpenAI or a named \
                             OpenAI-compatible); use chat/completions for \
                             cross-vendor chat mapping";
        if let Self::OpenAI(oi) = &source_endpoint
            && !matches!(oi, OpenAI::ChatCompletions(_))
        {
            match target_provider {
                InferenceProvider::OpenAI | InferenceProvider::Named(_) => {}
                _ => {
                    return Err(InvalidRequestError::UnsupportedEndpoint(CROSS.to_string()));
                }
            }
        }
        match (source_endpoint, target_provider) {
            (Self::OpenAI(o), InferenceProvider::Anthropic) => {
                Ok(Self::Anthropic(Anthropic::from(o)))
            }
            (Self::OpenAI(o), InferenceProvider::OpenAI) => Ok(Self::OpenAI(o)),
            (Self::OpenAI(o), InferenceProvider::GoogleGemini) => Ok(Self::Google(Google::from(o))),
            (Self::OpenAI(o), InferenceProvider::Ollama) => Ok(Self::Ollama(Ollama::from(o))),
            (Self::OpenAI(o), InferenceProvider::Bedrock) => Ok(Self::Bedrock(Bedrock::from(o))),
            (Self::OpenAI(o), InferenceProvider::Named(name)) => Ok(Self::OpenAICompatible {
                provider: InferenceProvider::Named(name.clone()),
                openai_endpoint: o,
            }),
            (Self::OpenAI(o), InferenceProvider::Custom) => Ok(Self::OpenAICompatible {
                provider: InferenceProvider::Custom,
                openai_endpoint: o,
            }),
            _ => Err(InvalidRequestError::UnsupportedProvider(
                target_provider.clone(),
            )),
        }
    }

    #[must_use]
    pub fn provider(&self) -> InferenceProvider {
        match self {
            Self::OpenAI(_) => InferenceProvider::OpenAI,
            Self::Anthropic(_) => InferenceProvider::Anthropic,
            Self::Google(_) => InferenceProvider::GoogleGemini,
            Self::Ollama(_) => InferenceProvider::Ollama,
            Self::Bedrock(_) => InferenceProvider::Bedrock,
            Self::OpenAICompatible { provider, .. } => provider.clone(),
        }
    }

    pub fn path(
        &self,
        model_id: Option<&ModelId>,
        is_stream: bool,
    ) -> Result<String, InternalError> {
        match self {
            Self::OpenAI(openai) => Ok(openai.path().to_string()),
            Self::OpenAICompatible {
                openai_endpoint, ..
            } => Ok(openai_endpoint.path().to_string()),
            Self::Anthropic(anthropic) => Ok(anthropic.path().to_string()),
            Self::Google(google) => Ok(google.path().to_string()),
            Self::Ollama(ollama) => Ok(ollama.path().to_string()),
            Self::Bedrock(bedrock) => {
                if let Some(model_id) = model_id {
                    Ok(bedrock.path(model_id, is_stream))
                } else {
                    tracing::error!("Bedrock path requires model id");
                    Err(InternalError::Internal)
                }
            }
        }
    }

    #[must_use]
    pub fn endpoint_type(&self) -> EndpointType {
        match self {
            Self::OpenAI(openai) => openai.endpoint_type(),
            Self::OpenAICompatible {
                openai_endpoint, ..
            } => openai_endpoint.endpoint_type(),
            Self::Anthropic(anthropic) => anthropic.endpoint_type(),
            Self::Google(google) => google.endpoint_type(),
            Self::Ollama(ollama) => ollama.endpoint_type(),
            Self::Bedrock(bedrock) => bedrock.endpoint_type(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::AsRefStr)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum EndpointType {
    Chat,
    /// OpenAI `embeddings` and similar; split from chat payloads for
    /// `load_balance` / observability
    Embeddings,
    Image,
    Audio,
}
