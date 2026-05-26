//! Google AI (Gemini) endpoint definitions.

pub mod generate_contents;

use super::EndpointType;
pub use generate_contents::GenerateContents;

/// Google AI endpoint variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter)]
pub enum Google {
    /// Generate content endpoint (Gemini).
    GenerateContents(GenerateContents),
}

impl Google {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::GenerateContents(_) => "generateContent",
        }
    }

    #[must_use]
    pub fn generate_contents() -> Self {
        Self::GenerateContents(GenerateContents)
    }

    #[must_use]
    pub fn endpoint_type(&self) -> EndpointType {
        match self {
            Self::GenerateContents(_) => EndpointType::Chat,
        }
    }
}
