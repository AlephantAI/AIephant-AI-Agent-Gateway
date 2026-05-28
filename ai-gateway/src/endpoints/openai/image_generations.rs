use async_openai::types::{CreateImageRequest, ImageModel, ImagesResponse};

use crate::{
    endpoints::{AiRequest, Endpoint},
    error::mapper::MapperError,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ImageGenerations;

impl Endpoint for ImageGenerations {
    const PATH: &'static str = "v1/images/generations";
    type RequestBody = CreateImageRequest;
    type ResponseBody = ImagesResponse;
    type StreamResponseBody = ImagesResponse;
    type ErrorResponseBody = async_openai::error::WrappedError;
}

impl AiRequest for CreateImageRequest {
    fn is_stream(&self) -> bool {
        false
    }

    fn model(&self) -> Result<ModelId, MapperError> {
        let m = self.model.as_ref().ok_or_else(|| {
            MapperError::InvalidModelName(
                "unified API images/generations require `model` for routing".to_string(),
            )
        })?;
        let name = match m {
            ImageModel::DallE2 => "dall-e-2".to_string(),
            ImageModel::DallE3 => "dall-e-3".to_string(),
            ImageModel::Other(s) => s.clone(),
        };
        ModelId::from_str_and_provider(InferenceProvider::OpenAI, &name)
    }
}
