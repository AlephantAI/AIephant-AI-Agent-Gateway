use crate::endpoints::{
    anthropic::Anthropic, bedrock::Bedrock, google::Google, ollama::Ollama,
    openai::OpenAI,
};

impl From<Anthropic> for OpenAI {
    fn from(value: Anthropic) -> Self {
        match value {
            Anthropic::Messages(_) => Self::chat_completions(),
        }
    }
}

impl From<OpenAI> for Anthropic {
    fn from(value: OpenAI) -> Self {
        match value {
            OpenAI::ChatCompletions(_) => Self::messages(),
            OpenAI::Completions(_)
            | OpenAI::Embeddings(_)
            | OpenAI::ImageGenerations(_)
            | OpenAI::Responses(_) => {
                unreachable!(
                    "ApiEndpoint::mapped only maps ChatCompletions to \
                     Anthropic"
                )
            }
        }
    }
}

impl From<Google> for OpenAI {
    fn from(value: Google) -> Self {
        match value {
            Google::GenerateContents(_) => Self::chat_completions(),
        }
    }
}

impl From<OpenAI> for Google {
    fn from(value: OpenAI) -> Self {
        match value {
            OpenAI::ChatCompletions(_) => Self::generate_contents(),
            OpenAI::Completions(_)
            | OpenAI::Embeddings(_)
            | OpenAI::ImageGenerations(_)
            | OpenAI::Responses(_) => {
                unreachable!(
                    "ApiEndpoint::mapped only maps ChatCompletions to Google"
                )
            }
        }
    }
}

impl From<OpenAI> for Ollama {
    fn from(value: OpenAI) -> Self {
        match value {
            OpenAI::ChatCompletions(_) => Self::chat_completions(),
            OpenAI::Completions(_)
            | OpenAI::Embeddings(_)
            | OpenAI::ImageGenerations(_)
            | OpenAI::Responses(_) => {
                unreachable!(
                    "ApiEndpoint::mapped only maps ChatCompletions to Ollama"
                )
            }
        }
    }
}

impl From<Ollama> for OpenAI {
    fn from(value: Ollama) -> Self {
        match value {
            Ollama::ChatCompletions(_) => Self::chat_completions(),
        }
    }
}
impl From<OpenAI> for Bedrock {
    fn from(value: OpenAI) -> Self {
        match value {
            OpenAI::ChatCompletions(_) => Self::converse(),
            OpenAI::Completions(_)
            | OpenAI::Embeddings(_)
            | OpenAI::ImageGenerations(_)
            | OpenAI::Responses(_) => {
                unreachable!(
                    "ApiEndpoint::mapped only maps ChatCompletions to Bedrock"
                )
            }
        }
    }
}
