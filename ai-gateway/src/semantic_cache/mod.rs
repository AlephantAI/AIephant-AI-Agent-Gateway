pub mod base_url_resolver;
pub mod embedder_client;
pub mod embedding_identity;
pub mod header_policy;
pub mod key_builder;
pub mod qdrant_store;
pub mod service;

pub use base_url_resolver::EmbeddingBaseUrlResolver;
pub use embedder_client::OpenAiEmbedderClient;
pub use embedding_identity::{
    EmbeddingIdentity, collection_name_for_embedding, parse_embedding_identity,
};
pub use header_policy::SemanticPolicy;
pub use key_builder::{
    BuiltKey, build_cache_key, extract_embed_text_from_body,
};
pub use qdrant_store::{
    QdrantEnsureCollection, QdrantStore, SemanticVectorHit,
};
pub use service::{
    PreparedSemanticRequest, SemanticCacheService, SemanticHit,
    SemanticLookupOutcome, SemanticLookupRequest, SemanticWriteContext,
    SemanticWriteRequest,
};
