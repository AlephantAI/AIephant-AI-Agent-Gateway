#![allow(clippy::large_futures)]

use std::{collections::HashMap, net::TcpListener};

use ai_gateway::{
    app::AppResponse,
    config::{Config, alephant::AlephantFeatures, providers::GlobalProviderConfig},
    crypto::master_key,
    tests::{TestDefault, harness::Harness, mock::MockArgs},
    types::{provider::InferenceProvider, secret::Secret},
    virtual_key::legacy_key::hash_key,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use indexmap::IndexSet;
use serde_json::{Value, json};
use sqlx::PgPool;
use stubr::wiremock_rs::{
    Mock, ResponseTemplate,
    matchers::{header, method, path},
};
use tower::Service;
use uuid::Uuid;

const MASTER_KEY_ENCRYPTION_KEY_ENV: &str = "MASTER_KEY_ENCRYPTION_KEY";
const TEST_MASTER_KEY_ENCRYPTION_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
const DEFAULT_TEST_DB_URL: &str = "postgres://postgres:postgres@localhost:54322/postgres";
const TEST_WORKSPACE_ID: &str = "f9e87d88-39f3-42ef-b485-4991737db6cf";
const TEST_WORKSPACE_OWNER_ID: &str = "538b199a-b286-4a23-9f46-b57f02075341";
const REQUEST_URI: &str = "http://router.alephant.test/v1/chat/completions";
const AWS_ACCESS_KEY_ID_ENV: &str = "AWS_ACCESS_KEY_ID";
const AWS_SECRET_ACCESS_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
const TEST_BEDROCK_ACCESS_KEY: &str = "bedrock-access-key-test";
const TEST_BEDROCK_SECRET_KEY: &str = "bedrock-secret-key-test";
const OPENAI_STREAM_RESPONSE_BODY: &str = concat!(
    "data: {",
    "\"id\":\"chatcmpl-stream-1\",",
    "\"object\":\"chat.completion.chunk\",",
    "\"created\":1741569952,",
    "\"model\":\"gpt-4.1-2025-04-14\",",
    "\"choices\":[{\"index\":0,",
    "\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},",
    "\"finish_reason\":null}",
    "]}\n\n",
    "data: {",
    "\"id\":\"chatcmpl-stream-1\",",
    "\"object\":\"chat.completion.chunk\",",
    "\"created\":1741569952,",
    "\"model\":\"gpt-4.1-2025-04-14\",",
    "\"choices\":[{\"index\":0,",
    "\"delta\":{},\"finish_reason\":\"stop\"}]}",
    "\n\n",
    "data: [DONE]\n\n"
);
const ANTHROPIC_STREAM_RESPONSE_BODY: &str = concat!(
    "data: {",
    "\"type\":\"message_start\",",
    "\"message\":{",
    "\"id\":\"msg-stream-1\",",
    "\"type\":\"message\",",
    "\"role\":\"assistant\",",
    "\"content\":[],",
    "\"model\":\"claude-sonnet-4-0\",",
    "\"stop_reason\":null,",
    "\"stop_sequence\":null,",
    "\"usage\":{\"input_tokens\":10,\"output_tokens\":0}",
    "}}",
    "\n\n",
    "data: {",
    "\"type\":\"content_block_start\",",
    "\"index\":0,",
    "\"content_block\":{\"type\":\"text\",\"text\":\"\"}",
    "}",
    "\n\n",
    "data: {",
    "\"type\":\"content_block_delta\",",
    "\"index\":0,",
    "\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}",
    "}",
    "\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}",
    "\n\n",
    "data: {",
    "\"type\":\"message_delta\",",
    "\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},",
    "\"usage\":{\"input_tokens\":10,\"output_tokens\":1}",
    "}",
    "\n\n",
    "data: {\"type\":\"message_stop\"}",
    "\n\n"
);

struct UnifiedApiCase {
    model: &'static str,
    provider_code: &'static str,
    provider_name: &'static str,
    api_key: &'static str,
    master_key_plaintext: &'static str,
    stub_id: &'static str,
    mock_target: MockTarget,
    master_key_id: &'static str,
    virtual_key_id: &'static str,
}

impl UnifiedApiCase {
    fn model_provider_prefix(&self) -> &'static str {
        self.model.split_once('/').expect("provider/model format").0
    }

    fn raw_model_id(&self) -> &'static str {
        self.model.split_once('/').expect("provider/model format").1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MockTarget {
    OpenAICompatible,
    Anthropic,
    Gemini,
    Ollama,
    Bedrock,
    Mistral,
}

#[derive(Clone)]
struct ProviderSnapshot {
    id: Uuid,
    existed: bool,
    default_base_url: Option<String>,
    enabled: bool,
}

#[derive(Clone, Copy)]
struct ProviderModelSnapshot {
    existed: bool,
    enabled: bool,
}

#[derive(Clone)]
struct SeedSnapshot {
    provider: ProviderSnapshot,
    model: ProviderModelSnapshot,
    master_key_id: Uuid,
    virtual_key_id: Uuid,
}

const OPENAI_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "openai/gpt-4o-mini",
    provider_code: "openai",
    provider_name: "OpenAI",
    api_key: "sk-unified-test-openai",
    master_key_plaintext: "sk-provider-openai-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000001",
    virtual_key_id: "72000000-0000-0000-0000-000000000001",
};

const ANTHROPIC_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "anthropic/claude-sonnet-4-0",
    provider_code: "anthropic",
    provider_name: "Anthropic",
    api_key: "sk-unified-test-anthropic",
    master_key_plaintext: "sk-provider-anthropic-test",
    stub_id: "success:anthropic:messages",
    mock_target: MockTarget::Anthropic,
    master_key_id: "71000000-0000-0000-0000-000000000002",
    virtual_key_id: "72000000-0000-0000-0000-000000000002",
};

const GEMINI_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "gemini/gemini-2.0-flash",
    provider_code: "google",
    provider_name: "Google Gemini",
    api_key: "sk-unified-test-gemini",
    master_key_plaintext: "sk-provider-gemini-test",
    stub_id: "success:gemini:generate_content",
    mock_target: MockTarget::Gemini,
    master_key_id: "71000000-0000-0000-0000-000000000003",
    virtual_key_id: "72000000-0000-0000-0000-000000000003",
};

const OLLAMA_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "ollama/llama3",
    provider_code: "ollama",
    provider_name: "Ollama",
    api_key: "sk-unified-test-ollama",
    master_key_plaintext: "sk-provider-ollama-test",
    stub_id: "success:ollama:chat_completions",
    mock_target: MockTarget::Ollama,
    master_key_id: "71000000-0000-0000-0000-000000000004",
    virtual_key_id: "72000000-0000-0000-0000-000000000004",
};

const BEDROCK_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
    provider_code: "bedrock",
    provider_name: "AWS Bedrock",
    api_key: "sk-unified-test-bedrock",
    master_key_plaintext: "sk-provider-bedrock-test",
    stub_id: "success:bedrock:converse",
    mock_target: MockTarget::Bedrock,
    master_key_id: "71000000-0000-0000-0000-000000000005",
    virtual_key_id: "72000000-0000-0000-0000-000000000005",
};

const DEEPSEEK_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "deepseek/deepseek-chat",
    provider_code: "deepseek",
    provider_name: "DeepSeek",
    api_key: "sk-unified-test-deepseek",
    master_key_plaintext: "sk-provider-deepseek-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000006",
    virtual_key_id: "72000000-0000-0000-0000-000000000006",
};

const DEEPSEEK_REASONER_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "deepseek/deepseek-reasoner",
    provider_code: "deepseek",
    provider_name: "DeepSeek",
    api_key: "sk-unified-test-deepseek-reasoner",
    master_key_plaintext: "sk-provider-deepseek-reasoner-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000014",
    virtual_key_id: "72000000-0000-0000-0000-000000000014",
};

const QWEN_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "qwen/qwen3-32b",
    provider_code: "qwen",
    provider_name: "Qwen",
    api_key: "sk-unified-test-qwen",
    master_key_plaintext: "sk-provider-qwen-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000008",
    virtual_key_id: "72000000-0000-0000-0000-000000000008",
};

const MINIMAX_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "minimax/minimax-m1",
    provider_code: "minimax",
    provider_name: "MiniMax",
    api_key: "sk-unified-test-minimax",
    master_key_plaintext: "sk-provider-minimax-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000009",
    virtual_key_id: "72000000-0000-0000-0000-000000000009",
};

const MOONSHOTAI_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "moonshotai/kimi-k2-instruct",
    provider_code: "moonshotai",
    provider_name: "MoonshotAI",
    api_key: "sk-unified-test-moonshotai",
    master_key_plaintext: "sk-provider-moonshotai-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000010",
    virtual_key_id: "72000000-0000-0000-0000-000000000010",
};

const MISTRAL_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "mistral/mistral-large-latest",
    provider_code: "mistral",
    provider_name: "Mistral",
    api_key: "sk-unified-test-mistral",
    master_key_plaintext: "sk-provider-mistral-test",
    stub_id: "success:mistral:chat_completion",
    mock_target: MockTarget::Mistral,
    master_key_id: "71000000-0000-0000-0000-000000000007",
    virtual_key_id: "72000000-0000-0000-0000-000000000007",
};

const GROQ_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "groq/llama-3.3-70b-versatile",
    provider_code: "groq",
    provider_name: "Groq",
    api_key: "sk-unified-test-groq",
    master_key_plaintext: "sk-provider-groq-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000011",
    virtual_key_id: "72000000-0000-0000-0000-000000000011",
};

const XAI_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "xai/grok-4",
    provider_code: "xai",
    provider_name: "xAI",
    api_key: "sk-unified-test-xai",
    master_key_plaintext: "sk-provider-xai-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000012",
    virtual_key_id: "72000000-0000-0000-0000-000000000012",
};

const HYPERBOLIC_CASE: UnifiedApiCase = UnifiedApiCase {
    model: "hyperbolic/meta-llama/Meta-Llama-3.1-70B-Instruct",
    provider_code: "hyperbolic",
    provider_name: "Hyperbolic",
    api_key: "sk-unified-test-hyperbolic",
    master_key_plaintext: "sk-provider-hyperbolic-test",
    stub_id: "success:openai:chat_completion",
    mock_target: MockTarget::OpenAICompatible,
    master_key_id: "71000000-0000-0000-0000-000000000013",
    virtual_key_id: "72000000-0000-0000-0000-000000000013",
};

// Keep this manifest aligned with
// docs/human-docs/provider-onboarding-template.md. These cases are the current
// examples for the standard OpenAI-compatible named provider onboarding suite:
// one case constant plus the same five Unified API regressions.
const STANDARD_OPENAI_COMPATIBLE_NAMED_PROVIDER_CASES: [&UnifiedApiCase; 6] = [
    &QWEN_CASE,
    &MINIMAX_CASE,
    &MOONSHOTAI_CASE,
    &GROQ_CASE,
    &XAI_CASE,
    &HYPERBOLIC_CASE,
];

fn assert_openai_compatible_named_provider_manifest(case: &UnifiedApiCase) {
    assert_eq!(
        case.model_provider_prefix(),
        case.provider_code,
        "{} should use provider/model naming",
        case.provider_name
    );
    assert_eq!(
        case.mock_target,
        MockTarget::OpenAICompatible,
        "{} should stay on the OpenAI-compatible mock/runtime path",
        case.provider_name
    );
    assert_eq!(
        case.stub_id, "success:openai:chat_completion",
        "{} should reuse the OpenAI-compatible upstream contract",
        case.provider_name
    );
    assert!(
        !case.raw_model_id().is_empty(),
        "{} should declare a raw model id",
        case.provider_name
    );
}

#[test]
fn standard_openai_compatible_named_provider_onboarding_manifest_is_valid() {
    for case in STANDARD_OPENAI_COMPATIBLE_NAMED_PROVIDER_CASES {
        assert_openai_compatible_named_provider_manifest(case);
    }
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn harness_shutdown_closes_router_store_pool() {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let mut config = Config::test_default();
    use_test_database_url(&mut config);
    ensure_mock_provider_configs(&mut config);
    config.alephant.features = AlephantFeatures::All;

    let harness = Harness::builder()
        .with_config(config)
        .with_mock_args(MockArgs::builder().openai_port(port).verify(false).build())
        .with_mock_auth()
        .build()
        .await;

    let pool = harness
        .app_factory
        .state
        .router_store()
        .expect("router_store should be initialized for test harness")
        .pool
        .clone();

    assert!(
        !pool.is_closed(),
        "router_store pool should start open before shutdown"
    );

    harness.shutdown().await;

    assert!(
        pool.is_closed(),
        "router_store pool should close on shutdown"
    );
}

struct MasterKeyGuard {
    previous: Option<String>,
}

impl MasterKeyGuard {
    fn set() -> Self {
        let previous = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV).ok();
        unsafe {
            std::env::set_var(
                MASTER_KEY_ENCRYPTION_KEY_ENV,
                TEST_MASTER_KEY_ENCRYPTION_KEY_B64,
            );
        }
        Self { previous }
    }
}

impl Drop for MasterKeyGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(MASTER_KEY_ENCRYPTION_KEY_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(MASTER_KEY_ENCRYPTION_KEY_ENV);
            },
        }
    }
}

struct AwsCredsGuard {
    previous_access_key: Option<String>,
    previous_secret_key: Option<String>,
}

impl AwsCredsGuard {
    fn set() -> Self {
        let previous_access_key = std::env::var(AWS_ACCESS_KEY_ID_ENV).ok();
        let previous_secret_key = std::env::var(AWS_SECRET_ACCESS_KEY_ENV).ok();
        unsafe {
            std::env::set_var(AWS_ACCESS_KEY_ID_ENV, TEST_BEDROCK_ACCESS_KEY);
            std::env::set_var(AWS_SECRET_ACCESS_KEY_ENV, TEST_BEDROCK_SECRET_KEY);
        }
        Self {
            previous_access_key,
            previous_secret_key,
        }
    }
}

impl Drop for AwsCredsGuard {
    fn drop(&mut self) {
        match &self.previous_access_key {
            Some(value) => unsafe {
                std::env::set_var(AWS_ACCESS_KEY_ID_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(AWS_ACCESS_KEY_ID_ENV);
            },
        }
        match &self.previous_secret_key {
            Some(value) => unsafe {
                std::env::set_var(AWS_SECRET_ACCESS_KEY_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(AWS_SECRET_ACCESS_KEY_ENV);
            },
        }
    }
}

fn parse_uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("valid UUID constant")
}

fn workspace_id() -> Uuid {
    parse_uuid(TEST_WORKSPACE_ID)
}

fn workspace_owner_id() -> Uuid {
    parse_uuid(TEST_WORKSPACE_OWNER_ID)
}

fn preferred_test_db_url(default_url: &str) -> String {
    std::env::var("POSTGRES_DATABASE_URL")
        .or_else(|_| std::env::var("AI_GATEWAY__DATABASE__URL"))
        .unwrap_or_else(|_| default_url.to_string())
}

fn test_db_url() -> String {
    preferred_test_db_url(DEFAULT_TEST_DB_URL)
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn master_key_encryption_key() -> [u8; 32] {
    let encoded = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV)
        .expect("MASTER_KEY_ENCRYPTION_KEY should be set by test guard");
    let decoded = STANDARD
        .decode(encoded)
        .expect("MASTER_KEY_ENCRYPTION_KEY must be valid base64");
    decoded
        .try_into()
        .expect("MASTER_KEY_ENCRYPTION_KEY must decode to 32 bytes")
}

fn masked_key(plaintext: &str) -> String {
    let suffix = if plaintext.len() > 4 {
        &plaintext[plaintext.len() - 4..]
    } else {
        plaintext
    };
    format!("sk-...{suffix}")
}

fn ensure_mock_provider_configs(config: &mut Config) {
    for (provider, base_url) in [
        (InferenceProvider::OpenAI, "http://127.0.0.1:1/"),
        (InferenceProvider::Anthropic, "http://127.0.0.1:1/"),
        (InferenceProvider::GoogleGemini, "http://127.0.0.1:1/"),
        (InferenceProvider::Ollama, "http://127.0.0.1:1/"),
        (InferenceProvider::Bedrock, "http://127.0.0.1:1/"),
        (
            InferenceProvider::Named("mistral".into()),
            "http://127.0.0.1:1/",
        ),
    ] {
        config
            .providers
            .entry(provider.clone())
            .or_insert_with(|| GlobalProviderConfig {
                models: IndexSet::new(),
                base_url: base_url.parse().expect("valid mock base url"),
                cn_base_url: None,
                version: None,
                upstream_auth: Default::default(),
            });
    }
}

fn use_test_database_url(config: &mut Config) {
    config.database.url = Secret::from(test_db_url());
}

async fn db_pool() -> PgPool {
    PgPool::connect(&test_db_url())
        .await
        .expect("failed to connect test database")
}

async fn ensure_test_workspace(pool: &PgPool) -> Result<(), sqlx::Error> {
    let owner_id = workspace_owner_id();

    sqlx::query(
        r"INSERT INTO users
            (id, email, password_hash, status, display_name, created_at, updated_at)
          VALUES
            ($1, 'gateway-unified-api-test-owner@example.invalid', 'test-password-hash', 'active', 'Gateway Unified API Test Owner', now(), now())
          ON CONFLICT (id) DO UPDATE
            SET status = 'active',
                display_name = EXCLUDED.display_name,
                updated_at = now()",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;

    sqlx::query(
        r"INSERT INTO workspaces
            (id, name, slug, type, owner_id, settings, created_at, updated_at, deleted_at)
          VALUES
            ($1, 'Gateway Unified API Test Workspace', 'gateway-unified-api-test-workspace', 'personal'::workspace_type_enum, $2, '{}'::jsonb, now(), now(), NULL)
          ON CONFLICT (id) DO UPDATE
            SET name = EXCLUDED.name,
                owner_id = EXCLUDED.owner_id,
                settings = EXCLUDED.settings,
                deleted_at = NULL,
                updated_at = now()",
    )
    .bind(workspace_id())
    .bind(owner_id)
    .execute(pool)
    .await?;

    Ok(())
}

async fn upsert_provider(
    pool: &PgPool,
    case: &UnifiedApiCase,
    base_url: &str,
) -> Result<ProviderSnapshot, sqlx::Error> {
    let existing = sqlx::query_as::<_, (Uuid, Option<String>, bool)>(
        r"SELECT id, default_base_url, enabled
          FROM providers
         WHERE code = $1",
    )
    .bind(case.provider_code)
    .fetch_optional(pool)
    .await?;

    let snapshot = if let Some((id, default_base_url, enabled)) = existing {
        ProviderSnapshot {
            id,
            existed: true,
            default_base_url,
            enabled,
        }
    } else {
        ProviderSnapshot {
            id: Uuid::new_v4(),
            existed: false,
            default_base_url: None,
            enabled: false,
        }
    };

    sqlx::query(
        r"INSERT INTO providers
            (id, code, name, default_base_url, sort_order, enabled, created_at, updated_at)
          VALUES ($1, $2, $3, $4, 999, true, now(), now())
          ON CONFLICT (code) DO UPDATE
            SET default_base_url = EXCLUDED.default_base_url,
                enabled = true,
                updated_at = now()",
    )
    .bind(snapshot.id)
    .bind(case.provider_code)
    .bind(case.provider_name)
    .bind(base_url)
    .execute(pool)
    .await?;

    Ok(snapshot)
}

async fn upsert_provider_model(
    pool: &PgPool,
    provider_id: Uuid,
    case: &UnifiedApiCase,
) -> Result<ProviderModelSnapshot, sqlx::Error> {
    let model_id = case.model.split_once('/').expect("provider/model format").1;
    let existing = sqlx::query_as::<_, (bool,)>(
        r"SELECT enabled
          FROM provider_models
         WHERE provider_id = $1
           AND model_id = $2",
    )
    .bind(provider_id)
    .bind(model_id)
    .fetch_optional(pool)
    .await?;

    let snapshot = if let Some((enabled,)) = existing {
        ProviderModelSnapshot {
            existed: true,
            enabled,
        }
    } else {
        ProviderModelSnapshot {
            existed: false,
            enabled: false,
        }
    };

    sqlx::query(
        r"INSERT INTO provider_models
            (id, provider_id, model_id, display_name, enabled, created_at, updated_at)
          VALUES ($1, $2, $3, $4, true, now(), now())
          ON CONFLICT (provider_id, model_id) DO UPDATE
            SET enabled = true,
                updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(provider_id)
    .bind(model_id)
    .bind(model_id)
    .execute(pool)
    .await?;

    Ok(snapshot)
}

async fn upsert_master_key(
    pool: &PgPool,
    provider_id: Uuid,
    case: &UnifiedApiCase,
) -> Result<Uuid, sqlx::Error> {
    let master_key_id = parse_uuid(case.master_key_id);
    let enc_key = master_key_encryption_key();
    let (ciphertext, nonce) = master_key::encrypt(case.master_key_plaintext.as_bytes(), &enc_key)
        .expect("encrypt master key");

    sqlx::query(
        r"INSERT INTO master_keys
            (id, workspace_id, label, provider_id, key_ciphertext, key_nonce, masked_key, base_url, status, deleted_at, created_at, updated_at)
          VALUES
            ($1, $2, $3, $4, $5, $6, $7, NULL, 'active'::api_key_status_enum, NULL, now(), now())
          ON CONFLICT (id) DO UPDATE
            SET workspace_id = EXCLUDED.workspace_id,
                label = EXCLUDED.label,
                provider_id = EXCLUDED.provider_id,
                key_ciphertext = EXCLUDED.key_ciphertext,
                key_nonce = EXCLUDED.key_nonce,
                masked_key = EXCLUDED.masked_key,
                base_url = NULL,
                status = 'active'::api_key_status_enum,
                deleted_at = NULL,
                updated_at = now()",
    )
    .bind(master_key_id)
    .bind(workspace_id())
    .bind(format!("unified-api-{}", case.provider_code))
    .bind(provider_id)
    .bind(ciphertext)
    .bind(nonce)
    .bind(masked_key(case.master_key_plaintext))
    .execute(pool)
    .await?;

    Ok(master_key_id)
}

async fn upsert_virtual_key(
    pool: &PgPool,
    master_key_id: Uuid,
    case: &UnifiedApiCase,
) -> Result<Uuid, sqlx::Error> {
    let virtual_key_id = parse_uuid(case.virtual_key_id);
    let key_hash = hash_key(case.api_key);
    let key_prefix: String = case.api_key.chars().take(16).collect();

    sqlx::query(
        r"INSERT INTO virtual_keys
            (id, workspace_id, master_key_id, label, key_hash, key_prefix, status, deleted_at, created_at, updated_at)
          VALUES
            ($1, $2, $3, $4, $5, $6, 'active'::virtual_key_status_enum, NULL, now(), now())
          ON CONFLICT (id) DO UPDATE
            SET workspace_id = EXCLUDED.workspace_id,
                master_key_id = EXCLUDED.master_key_id,
                label = EXCLUDED.label,
                key_hash = EXCLUDED.key_hash,
                key_prefix = EXCLUDED.key_prefix,
                status = 'active'::virtual_key_status_enum,
                deleted_at = NULL,
                updated_at = now()",
    )
    .bind(virtual_key_id)
    .bind(workspace_id())
    .bind(master_key_id)
    .bind(format!("unified-api-{}", case.provider_code))
    .bind(key_hash)
    .bind(key_prefix)
    .execute(pool)
    .await?;

    Ok(virtual_key_id)
}

async fn seed_case(
    pool: &PgPool,
    case: &UnifiedApiCase,
    base_url: &str,
) -> Result<SeedSnapshot, sqlx::Error> {
    ensure_test_workspace(pool).await?;
    let provider = upsert_provider(pool, case, base_url).await?;
    let model = upsert_provider_model(pool, provider.id, case).await?;
    let master_key_id = upsert_master_key(pool, provider.id, case).await?;
    let virtual_key_id = upsert_virtual_key(pool, master_key_id, case).await?;

    Ok(SeedSnapshot {
        provider,
        model,
        master_key_id,
        virtual_key_id,
    })
}

async fn cleanup_case(
    pool: &PgPool,
    case: &UnifiedApiCase,
    seed: SeedSnapshot,
) -> Result<(), sqlx::Error> {
    let model_id = case.model.split_once('/').expect("provider/model format").1;

    sqlx::query("DELETE FROM virtual_keys WHERE id = $1")
        .bind(seed.virtual_key_id)
        .execute(pool)
        .await?;

    sqlx::query("DELETE FROM master_keys WHERE id = $1")
        .bind(seed.master_key_id)
        .execute(pool)
        .await?;

    if seed.model.existed {
        sqlx::query(
            r"UPDATE provider_models
                SET enabled = $3,
                    updated_at = now()
              WHERE provider_id = $1
                AND model_id = $2",
        )
        .bind(seed.provider.id)
        .bind(model_id)
        .bind(seed.model.enabled)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r"DELETE FROM provider_models
              WHERE provider_id = $1
                AND model_id = $2",
        )
        .bind(seed.provider.id)
        .bind(model_id)
        .execute(pool)
        .await?;
    }

    if seed.provider.existed {
        sqlx::query(
            r"UPDATE providers
                SET default_base_url = $2,
                    enabled = $3,
                    updated_at = now()
              WHERE id = $1",
        )
        .bind(seed.provider.id)
        .bind(seed.provider.default_base_url)
        .bind(seed.provider.enabled)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM providers WHERE id = $1")
            .bind(seed.provider.id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

fn mock_args_for(case: &UnifiedApiCase, port: u16) -> MockArgs {
    let stubs = HashMap::from([
        (case.stub_id, 1.into()),
        ("success:s3:upload_request", 0.into()),
        ("success:alephant:log_request", (0_u64..).into()),
    ]);

    match case.mock_target {
        MockTarget::OpenAICompatible => MockArgs::builder().openai_port(port).stubs(stubs).build(),
        MockTarget::Anthropic => MockArgs::builder()
            .anthropic_port(port)
            .stubs(stubs)
            .build(),
        MockTarget::Gemini => MockArgs::builder().google_port(port).stubs(stubs).build(),
        MockTarget::Ollama => MockArgs::builder().ollama_port(port).stubs(stubs).build(),
        MockTarget::Bedrock => MockArgs::builder().bedrock_port(port).stubs(stubs).build(),
        MockTarget::Mistral => MockArgs::builder().mistral_port(port).stubs(stubs).build(),
    }
}

async fn response_parts(response: AppResponse) -> (StatusCode, String) {
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn run_unified_case(case: &UnifiedApiCase) -> Result<(StatusCode, String), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, case, &base_url)
        .await
        .map_err(|e| format!("seed failed for {}: {e}", case.provider_code))?;

    let mut config = Config::test_default();
    use_test_database_url(&mut config);
    ensure_mock_provider_configs(&mut config);
    config.alephant.features = AlephantFeatures::All;

    let mock_args = mock_args_for(case, port);

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;

    let run_result = async {
        let request_body = axum_core::body::Body::from(
            serde_json::to_vec(&json!({
                "model": case.model,
                "messages": [
                    {
                        "role": "user",
                        "content": "Hello, world!"
                    }
                ]
            }))
            .unwrap(),
        );

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", case.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        Ok(response_parts(response).await)
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, case, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for {}: {e}", case.provider_code));
    }

    run_result
}

async fn assert_unified_case_ok(case: &UnifiedApiCase) {
    let (status, body) = run_unified_case(case)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, StatusCode::OK, "response body: {body}");
}

async fn assert_openai_like_stream_case_ok(case: &UnifiedApiCase) {
    let request_payload = json!({
        "model": case.model,
        "stream": true,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });

    let (headers, body, requests) = run_openai_like_stream_case(case, request_payload)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let raw_model = case.raw_model_id();
    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    assert!(!body.contains("[DONE]"));
    assert_eq!(
        requests.len(),
        1,
        "expected one upstream {} request",
        case.provider_code
    );
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai-like body should be valid json");
    assert_eq!(upstream_body["model"], raw_model);
    assert_eq!(upstream_body["stream"], true);
}

async fn assert_openai_like_nonstream_case_ok(case: &UnifiedApiCase) {
    let request_payload = json!({
        "model": case.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let raw_model = case.raw_model_id();
    let response_payload = json!({
        "id": format!("chatcmpl-{}-test-1", case.provider_code),
        "object": "chat.completion",
        "created": 1748543700,
        "model": raw_model,
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": format!("Hello from {}.", case.provider_name)
                }
            }
        ],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "total_tokens": 12
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(case, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["model"], raw_model);
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        format!("Hello from {}.", case.provider_name)
    );

    assert_eq!(
        requests.len(),
        1,
        "expected one upstream {} request",
        case.provider_code
    );
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai-like body should be valid json");
    assert_eq!(upstream_body["model"], raw_model);
    assert_eq!(upstream_body["messages"][0]["content"], "Hello, world!");
}

async fn assert_openai_like_capability_fields_case_ok(case: &UnifiedApiCase) {
    let request_payload = json!({
        "model": case.model,
        "messages": [
            {
                "role": "user",
                "content": "Return a JSON response."
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "response_format": {
            "type": "json_object"
        }
    });
    let raw_model = case.raw_model_id();
    let response_payload = json!({
        "id": format!("chatcmpl-{}-capabilities-1", case.provider_code),
        "object": "chat.completion",
        "created": 1748543700,
        "model": raw_model,
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "{\"status\":\"ok\"}"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 6,
            "total_tokens": 16
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(case, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        requests.len(),
        1,
        "expected one upstream {} request",
        case.provider_code
    );
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai-like body should be valid json");
    assert_eq!(upstream_body["model"], raw_model);
    assert_eq!(upstream_body["parallel_tool_calls"], true);
    assert_eq!(upstream_body["response_format"]["type"], "json_object");
    assert_eq!(upstream_body["tool_choice"], "auto");
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
}

async fn assert_openai_like_tool_calls_case_ok(case: &UnifiedApiCase) {
    let request_payload = json!({
        "model": case.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let raw_model = case.raw_model_id();
    let response_payload = json!({
        "id": format!("chatcmpl-{}-tool-1", case.provider_code),
        "object": "chat.completion",
        "created": 1748543700,
        "model": raw_model,
        "choices": [
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": format!("call_{}_weather_1", case.provider_code),
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                }
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(case, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(
        requests.len(),
        1,
        "expected one upstream {} request",
        case.provider_code
    );
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai-like body should be valid json");
    assert_eq!(upstream_body["model"], raw_model);
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
}

async fn assert_openai_like_multimodal_case_ok(case: &UnifiedApiCase) {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": case.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let raw_model = case.raw_model_id();
    let response_payload = json!({
        "id": format!("chatcmpl-{}-image-1", case.provider_code),
        "object": "chat.completion",
        "created": 1748543700,
        "model": raw_model,
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "It looks like a test image."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 14,
            "completion_tokens": 8,
            "total_tokens": 22
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(case, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    assert_eq!(
        requests.len(),
        1,
        "expected one upstream {} request",
        case.provider_code
    );
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai-like body should be valid json");
    assert_eq!(upstream_body["model"], raw_model);
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
}

macro_rules! openai_compatible_named_provider_suite {
    (
        $nonstream:ident,
        $stream:ident,
        $tool_calls:ident,
        $multimodal:ident,
        $capability_fields:ident,
        $case:expr
    ) => {
        #[tokio::test]
        #[serial_test::serial(default_mock)]
        async fn $nonstream() {
            assert_openai_like_nonstream_case_ok(&$case).await;
        }

        #[tokio::test]
        #[serial_test::serial(default_mock)]
        async fn $stream() {
            assert_openai_like_stream_case_ok(&$case).await;
        }

        #[tokio::test]
        #[serial_test::serial(default_mock)]
        async fn $tool_calls() {
            assert_openai_like_tool_calls_case_ok(&$case).await;
        }

        #[tokio::test]
        #[serial_test::serial(default_mock)]
        async fn $multimodal() {
            assert_openai_like_multimodal_case_ok(&$case).await;
        }

        #[tokio::test]
        #[serial_test::serial(default_mock)]
        async fn $capability_fields() {
            assert_openai_like_capability_fields_case_ok(&$case).await;
        }
    };
}

async fn run_openai_stream_case() -> Result<(http::HeaderMap, String), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &OPENAI_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for openai stream: {e}"))?;

    let mut config = Config::test_default();
    use_test_database_url(&mut config);
    ensure_mock_provider_configs(&mut config);
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .openai_port(port)
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", (0_u64..).into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_STREAM_RESPONSE_BODY, "text/event-stream"),
        )
        .with_priority(1)
        .expect(1)
        .named("success:openai:chat_completion_stream_runtime")
        .mount(&harness.mock.openai_mock.http_server)
        .await;

    let run_result = async {
        let request_body = axum_core::body::Body::from(
            serde_json::to_vec(&json!({
                "model": OPENAI_CASE.model,
                "stream": true,
                "messages": [
                    {
                        "role": "user",
                        "content": "Hello, world!"
                    }
                ]
            }))
            .unwrap(),
        );

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", OPENAI_CASE.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Ok((headers, String::from_utf8_lossy(&body).into_owned()))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &OPENAI_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for openai stream: {e}"));
    }

    run_result
}

async fn run_anthropic_nonstream_runtime_case(
    request_payload: Value,
    response_payload: Value,
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &ANTHROPIC_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for anthropic runtime case: {e}"))?;

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .anthropic_port(port)
        .stubs(HashMap::from([
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
        .with_priority(1)
        .expect(1)
        .named("success:anthropic:messages_runtime")
        .mount(&harness.mock.anthropic_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!("Bearer {}", ANTHROPIC_CASE.api_key),
            )
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;
        let requests = harness
            .mock
            .anthropic_mock
            .http_server
            .received_requests_for("POST", "/v1/messages")
            .await
            .unwrap_or_default();
        Ok((status, body, requests))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &ANTHROPIC_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for anthropic runtime case: {e}"));
    }

    run_result
}

async fn run_gemini_nonstream_runtime_case(
    request_payload: Value,
    response_payload: Value,
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &GEMINI_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for gemini runtime case: {e}"))?;

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .google_port(port)
        .stubs(HashMap::from([
            ("success:gemini:generate_content", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path("/v1beta/openai/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
        .with_priority(1)
        .named("success:gemini:generate_content_runtime")
        .mount(&harness.mock.google_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", GEMINI_CASE.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;
        let requests = harness
            .mock
            .google_mock
            .http_server
            .received_requests_for("POST", "/v1beta/openai/chat/completions")
            .await
            .unwrap_or_default();
        Ok((status, body, requests))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &GEMINI_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for gemini runtime case: {e}"));
    }

    run_result
}

async fn run_bedrock_nonstream_runtime_case(
    request_payload: Value,
    response_payload: Value,
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let _aws_creds_guard = AwsCredsGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &BEDROCK_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for bedrock runtime case: {e}"))?;
    let upstream_path = "/model/anthropic.claude-3-5-sonnet-20240620-v1:0/converse";

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .bedrock_port(port)
        .stubs(HashMap::from([
            ("success:bedrock:converse", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path(upstream_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
        .with_priority(1)
        .named("success:bedrock:converse_runtime")
        .mount(&harness.mock.bedrock_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", BEDROCK_CASE.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;
        let requests = harness
            .mock
            .bedrock_mock
            .http_server
            .received_requests_for("POST", upstream_path)
            .await
            .unwrap_or_default();
        Ok((status, body, requests))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &BEDROCK_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for bedrock runtime case: {e}"));
    }

    run_result
}

async fn run_openai_like_nonstream_runtime_case(
    case: &UnifiedApiCase,
    request_payload: Value,
    response_payload: Value,
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    run_openai_like_nonstream_runtime_case_with_options(
        case,
        request_payload,
        response_payload,
        &[],
        None,
        None,
    )
    .await
}

async fn run_openai_like_nonstream_runtime_case_with_options(
    case: &UnifiedApiCase,
    request_payload: Value,
    response_payload: Value,
    extra_headers: &[(&str, &str)],
    allowed_models: Option<Vec<String>>,
    blocked_models: Option<Vec<String>>,
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, case, &base_url)
        .await
        .map_err(|e| format!("seed failed for {} runtime case: {e}", case.provider_code))?;
    if allowed_models.is_some() || blocked_models.is_some() {
        sqlx::query(
            r"UPDATE virtual_keys
                 SET allowed_models = $2,
                     blocked_models = $3,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(seed.virtual_key_id)
        .bind(allowed_models)
        .bind(blocked_models)
        .execute(&pool)
        .await
        .map_err(|e| format!("failed to update virtual key model policy: {e}"))?;
    }

    let mut config = Config::test_default();
    use_test_database_url(&mut config);
    ensure_mock_provider_configs(&mut config);
    config.alephant.features = AlephantFeatures::All;

    let mock_args = match case.mock_target {
        MockTarget::OpenAICompatible | MockTarget::Mistral => MockArgs::builder()
            .openai_port(port)
            .verify(false)
            .stubs(HashMap::from([
                (case.stub_id, 0.into()),
                ("success:s3:upload_request", 0.into()),
            ]))
            .build(),
        MockTarget::Ollama => MockArgs::builder()
            .ollama_port(port)
            .verify(false)
            .stubs(HashMap::from([
                (case.stub_id, 0.into()),
                ("success:s3:upload_request", 0.into()),
            ]))
            .build(),
        _ => {
            return Err(format!(
                "unsupported openai-like mock target for {}",
                case.provider_code
            ));
        }
    };

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let upstream_path = "/v1/chat/completions";
    match case.mock_target {
        MockTarget::OpenAICompatible | MockTarget::Mistral => {
            Mock::given(method("POST"))
                .and(path(upstream_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
                .with_priority(1)
                .named("success:openai_like_runtime")
                .mount(&harness.mock.openai_mock.http_server)
                .await;
        }
        MockTarget::Ollama => {
            Mock::given(method("POST"))
                .and(path(upstream_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
                .with_priority(1)
                .named("success:openai_like_runtime")
                .mount(&harness.mock.ollama_mock.http_server)
                .await;
        }
        _ => unreachable!(),
    }

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let mut request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", case.api_key));
        for (key, value) in extra_headers {
            request = request.header(*key, *value);
        }
        let request = request
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;
        let requests = match case.mock_target {
            MockTarget::OpenAICompatible | MockTarget::Mistral => harness
                .mock
                .openai_mock
                .http_server
                .received_requests_for("POST", upstream_path)
                .await
                .unwrap_or_default(),
            MockTarget::Ollama => harness
                .mock
                .ollama_mock
                .http_server
                .received_requests_for("POST", upstream_path)
                .await
                .unwrap_or_default(),
            _ => unreachable!(),
        };
        Ok((status, body, requests))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, case, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!(
            "cleanup failed for {} runtime case: {e}",
            case.provider_code
        ));
    }

    run_result
}

async fn upsert_provider_model_id(
    pool: &PgPool,
    provider_id: Uuid,
    model_id: &str,
) -> Result<ProviderModelSnapshot, sqlx::Error> {
    let existing = sqlx::query_as::<_, (bool,)>(
        r"SELECT enabled
          FROM provider_models
         WHERE provider_id = $1
           AND model_id = $2",
    )
    .bind(provider_id)
    .bind(model_id)
    .fetch_optional(pool)
    .await?;

    let snapshot = if let Some((enabled,)) = existing {
        ProviderModelSnapshot {
            existed: true,
            enabled,
        }
    } else {
        ProviderModelSnapshot {
            existed: false,
            enabled: false,
        }
    };

    sqlx::query(
        r"INSERT INTO provider_models
            (id, provider_id, model_id, display_name, enabled, created_at, updated_at)
          VALUES ($1, $2, $3, $4, true, now(), now())
          ON CONFLICT (provider_id, model_id) DO UPDATE
            SET enabled = true,
                updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(provider_id)
    .bind(model_id)
    .bind(model_id)
    .execute(pool)
    .await?;

    Ok(snapshot)
}

async fn cleanup_provider_model_id(
    pool: &PgPool,
    provider_id: Uuid,
    model_id: &str,
    snapshot: ProviderModelSnapshot,
) -> Result<(), sqlx::Error> {
    if snapshot.existed {
        sqlx::query(
            r"UPDATE provider_models
                SET enabled = $3,
                    updated_at = now()
              WHERE provider_id = $1
                AND model_id = $2",
        )
        .bind(provider_id)
        .bind(model_id)
        .bind(snapshot.enabled)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r"DELETE FROM provider_models
              WHERE provider_id = $1
                AND model_id = $2",
        )
        .bind(provider_id)
        .bind(model_id)
        .execute(pool)
        .await?;
    }

    Ok(())
}

async fn run_openai_large_context_runtime_case(
    request_payload: Value,
    response_payload: Value,
    extra_headers: &[(&str, &str)],
    extra_models: &[&str],
) -> Result<(StatusCode, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &OPENAI_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for large context runtime case: {e}"))?;

    let mut extra_snapshots = Vec::new();
    for model_id in extra_models {
        let snapshot = upsert_provider_model_id(&pool, seed.provider.id, model_id)
            .await
            .map_err(|e| format!("seed extra provider model failed for {model_id}: {e}"))?;
        extra_snapshots.push(((*model_id).to_string(), snapshot));
    }

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .openai_port(port)
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let upstream_path = "/v1/chat/completions";
    Mock::given(method("POST"))
        .and(path(upstream_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
        .with_priority(1)
        .named("success:openai:large_context_runtime")
        .mount(&harness.mock.openai_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let mut request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", OPENAI_CASE.api_key));
        for (key, value) in extra_headers {
            request = request.header(*key, *value);
        }

        let request = request
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;
        let requests = harness
            .mock
            .openai_mock
            .http_server
            .received_requests_for("POST", upstream_path)
            .await
            .unwrap_or_default();
        Ok((status, body, requests))
    }
    .await;

    harness.shutdown().await;

    for (model_id, snapshot) in extra_snapshots.into_iter().rev() {
        cleanup_provider_model_id(&pool, seed.provider.id, &model_id, snapshot)
            .await
            .map_err(|e| format!("cleanup extra provider model failed for {model_id}: {e}"))?;
    }

    let cleanup_result = cleanup_case(&pool, &OPENAI_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!(
            "cleanup failed for large context runtime case: {e}"
        ));
    }

    run_result
}

async fn run_openai_large_context_runtime_case_with_logs(
    request_payload: Value,
    response_payload: Value,
    extra_headers: &[(&str, &str)],
    extra_models: &[&str],
) -> Result<
    (
        StatusCode,
        String,
        Vec<stubr::wiremock_rs::Request>,
        Vec<stubr::wiremock_rs::Request>,
    ),
    String,
> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &OPENAI_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for large context runtime case: {e}"))?;

    let mut extra_snapshots = Vec::new();
    for model_id in extra_models {
        let snapshot = upsert_provider_model_id(&pool, seed.provider.id, model_id)
            .await
            .map_err(|e| format!("seed extra provider model failed for {model_id}: {e}"))?;
        extra_snapshots.push(((*model_id).to_string(), snapshot));
    }

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .openai_port(port)
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 1.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let upstream_path = "/v1/chat/completions";
    Mock::given(method("POST"))
        .and(path(upstream_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload))
        .with_priority(1)
        .named("success:openai:large_context_runtime_with_logs")
        .mount(&harness.mock.openai_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let mut request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", OPENAI_CASE.api_key));
        for (key, value) in extra_headers {
            request = request.header(*key, *value);
        }

        let request = request
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let (status, body) = response_parts(response).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let upstream_requests = harness
            .mock
            .openai_mock
            .http_server
            .received_requests_for("POST", upstream_path)
            .await
            .unwrap_or_default();
        let log_requests = harness
            .mock
            .alephant_mock
            .http_server
            .received_requests_for("POST", "/v1/log/request")
            .await
            .unwrap_or_default();
        Ok((status, body, upstream_requests, log_requests))
    }
    .await;

    harness.shutdown().await;

    for (model_id, snapshot) in extra_snapshots.into_iter().rev() {
        cleanup_provider_model_id(&pool, seed.provider.id, &model_id, snapshot)
            .await
            .map_err(|e| format!("cleanup extra provider model failed for {model_id}: {e}"))?;
    }

    let cleanup_result = cleanup_case(&pool, &OPENAI_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!(
            "cleanup failed for large context runtime case: {e}"
        ));
    }

    run_result
}

fn large_context_text(chars: usize) -> String {
    "large-context ".repeat(chars.div_ceil("large-context ".len()))[..chars].to_string()
}
async fn run_openai_like_stream_case(
    case: &UnifiedApiCase,
    request_payload: Value,
) -> Result<(http::HeaderMap, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, case, &base_url)
        .await
        .map_err(|e| format!("seed failed for {} stream case: {e}", case.provider_code))?;

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = match case.mock_target {
        MockTarget::OpenAICompatible | MockTarget::Mistral => MockArgs::builder()
            .openai_port(port)
            .stubs(HashMap::from([
                (case.stub_id, 0.into()),
                ("success:s3:upload_request", 0.into()),
                ("success:alephant:log_request", 0.into()),
            ]))
            .build(),
        MockTarget::Ollama => MockArgs::builder()
            .ollama_port(port)
            .stubs(HashMap::from([
                (case.stub_id, 0.into()),
                ("success:s3:upload_request", 0.into()),
                ("success:alephant:log_request", 0.into()),
            ]))
            .build(),
        _ => {
            return Err(format!(
                "unsupported openai-like stream target for {}",
                case.provider_code
            ));
        }
    };

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let upstream_path = "/v1/chat/completions";

    match case.mock_target {
        MockTarget::OpenAICompatible | MockTarget::Mistral => {
            Mock::given(method("POST"))
                .and(path(upstream_path))
                .and(header("accept", "text/event-stream"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(OPENAI_STREAM_RESPONSE_BODY, "text/event-stream"),
                )
                .with_priority(1)
                .named("success:openai_like_stream_runtime")
                .mount(&harness.mock.openai_mock.http_server)
                .await;
        }
        MockTarget::Ollama => {
            Mock::given(method("POST"))
                .and(path(upstream_path))
                .and(header("accept", "text/event-stream"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(OPENAI_STREAM_RESPONSE_BODY, "text/event-stream"),
                )
                .with_priority(1)
                .named("success:openai_like_stream_runtime")
                .mount(&harness.mock.ollama_mock.http_server)
                .await;
        }
        _ => unreachable!(),
    }

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", case.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let requests = match case.mock_target {
            MockTarget::OpenAICompatible | MockTarget::Mistral => harness
                .mock
                .openai_mock
                .http_server
                .received_requests_for("POST", upstream_path)
                .await
                .unwrap_or_default(),
            MockTarget::Ollama => harness
                .mock
                .ollama_mock
                .http_server
                .received_requests_for("POST", upstream_path)
                .await
                .unwrap_or_default(),
            _ => unreachable!(),
        };
        Ok((
            headers,
            String::from_utf8_lossy(&body).into_owned(),
            requests,
        ))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, case, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!(
            "cleanup failed for {} stream case: {e}",
            case.provider_code
        ));
    }

    run_result
}

async fn run_gemini_stream_case(
    request_payload: Value,
) -> Result<(http::HeaderMap, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &GEMINI_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for gemini stream: {e}"))?;

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .google_port(port)
        .stubs(HashMap::from([
            ("success:gemini:generate_content", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path("/v1beta/openai/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_STREAM_RESPONSE_BODY, "text/event-stream"),
        )
        .with_priority(1)
        .named("success:gemini:generate_content_stream_runtime")
        .mount(&harness.mock.google_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", GEMINI_CASE.api_key))
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let requests = harness
            .mock
            .google_mock
            .http_server
            .received_requests_for("POST", "/v1beta/openai/chat/completions")
            .await
            .unwrap_or_default();
        Ok((
            headers,
            String::from_utf8_lossy(&body).into_owned(),
            requests,
        ))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &GEMINI_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for gemini stream: {e}"));
    }

    run_result
}

async fn run_anthropic_stream_case(
    request_payload: Value,
) -> Result<(http::HeaderMap, String, Vec<stubr::wiremock_rs::Request>), String> {
    let _master_key_guard = MasterKeyGuard::set();
    let port = reserve_port();
    let base_url = format!("http://127.0.0.1:{port}/");
    let pool = db_pool().await;
    let seed = seed_case(&pool, &ANTHROPIC_CASE, &base_url)
        .await
        .map_err(|e| format!("seed failed for anthropic stream: {e}"))?;

    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .anthropic_port(port)
        .stubs(HashMap::from([
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(ANTHROPIC_STREAM_RESPONSE_BODY, "text/event-stream"),
        )
        .with_priority(1)
        .expect(1)
        .named("success:anthropic:messages_stream_runtime")
        .mount(&harness.mock.anthropic_mock.http_server)
        .await;

    let run_result = async {
        let request_body =
            axum_core::body::Body::from(serde_json::to_vec(&request_payload).unwrap());

        let request = Request::builder()
            .method(Method::POST)
            .uri(REQUEST_URI)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!("Bearer {}", ANTHROPIC_CASE.api_key),
            )
            .body(request_body)
            .map_err(|e| format!("request build failed: {e}"))?;

        let response = harness
            .call(request)
            .await
            .map_err(|_| "harness call failed".to_string())?;
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let requests = harness
            .mock
            .anthropic_mock
            .http_server
            .received_requests_for("POST", "/v1/messages")
            .await
            .unwrap_or_default();
        Ok((
            headers,
            String::from_utf8_lossy(&body).into_owned(),
            requests,
        ))
    }
    .await;

    harness.shutdown().await;

    let cleanup_result = cleanup_case(&pool, &ANTHROPIC_CASE, seed).await;
    pool.close().await;
    if let Err(e) = cleanup_result {
        return Err(format!("cleanup failed for anthropic stream: {e}"));
    }

    run_result
}

fn assert_openai_chat_completion_shape(body: &str) {
    let payload: Value = serde_json::from_str(body).expect("response should be valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["message"]["role"], "assistant");
    assert_eq!(payload["choices"][0]["finish_reason"], "stop");
    assert!(
        payload["model"]
            .as_str()
            .is_some_and(|model| !model.is_empty()),
        "response should contain a non-empty model field: {body}"
    );
    assert!(
        payload["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty()),
        "response should contain assistant content: {body}"
    );
}

fn assert_anthropic_openai_chat_completion_shape(body: &str) {
    let payload: Value = serde_json::from_str(body).expect("response should be valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["message"]["role"], "assistant");
    assert_eq!(payload["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "Hi! My name is Claude."
    );
    assert!(
        payload["model"]
            .as_str()
            .is_some_and(|model| !model.is_empty()),
        "response should contain a non-empty model field: {body}"
    );
}

/// Test that requests are properly passed through to the `OpenAI` provider
/// when using the /{provider} base url.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_unified_api() {
    let (status, body) = run_unified_case(&OPENAI_CASE)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_unified_api_stream() {
    let (headers, body) = run_openai_stream_case()
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(
        body.starts_with("data: "),
        "stream response should use SSE data prefix: {body}"
    );
    assert!(
        body.contains("\"object\":\"chat.completion.chunk\""),
        "stream response should contain chat.completion.chunk payload: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "stream response should preserve OpenAI-compatible [DONE] marker: \
         {body}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_unified_api_tool_calls() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-openai-tool-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_openai_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                }
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OPENAI_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o-mini");
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_strips_internal_headers_before_upstream() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [{"role": "user", "content": "Hello, headers."}]
    });
    let response_payload = json!({
        "id": "chatcmpl-header-sanitize",
        "object": "chat.completion",
        "created": 1748543700,
        "model": OPENAI_CASE.raw_model_id(),
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok"}
        }]
    });

    let (status, body, requests) = run_openai_like_nonstream_runtime_case_with_options(
        &OPENAI_CASE,
        request_payload,
        response_payload,
        &[
            ("alephant-api-key", "internal-gateway-key"),
            ("Alephant-Session-Id", "session-123"),
            ("Alephant-Session-Path", "workflow/unified"),
            ("Alephant-Session-Name", "Unified Planner"),
            ("Alephant-Embeddings-Key", "sk-embedding"),
            ("Alephant-Embeddings-Model", "openai/text-embedding-3-small"),
            ("Alephant-Cache-Semantic-Threshold", "0.95"),
            ("Alephant-Cache-Ttl", "60"),
            ("alephant-debug-headers", "true"),
            ("alephant-debug-body", "true"),
            ("x-keep-me", "alive"),
        ],
        None,
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_headers = &requests[0].headers;
    for stripped in [
        "alephant-api-key",
        "alephant-session-id",
        "alephant-session-path",
        "alephant-session-name",
        "alephant-embeddings-key",
        "alephant-embeddings-model",
        "alephant-cache-semantic-threshold",
        "alephant-cache-ttl",
        "alephant-debug-headers",
        "alephant-debug-body",
    ] {
        assert!(
            !upstream_headers
                .keys()
                .any(|name| name.as_str() == stripped),
            "{stripped} should not be forwarded upstream"
        );
    }
    assert_eq!(
        upstream_headers
            .iter()
            .find(|(name, _)| name.as_str() == "authorization")
            .and_then(|(_, values)| values.get(0))
            .map(|value| value.as_str()),
        Some("Bearer sk-provider-openai-test"),
    );
    assert!(
        upstream_headers
            .keys()
            .any(|name| name.as_str() == "x-keep-me")
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_explicit_unknown_model_reaches_openai_compatible_upstream() {
    let request_payload = json!({
        "model": "claude-sonnet-4.6",
        "messages": [
            {
                "role": "user",
                "content": "Hello, future model."
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-unknown-model-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "claude-sonnet-4.6",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "ok"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 4,
            "completion_tokens": 1,
            "total_tokens": 5
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OPENAI_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream body should be valid json");
    assert_eq!(upstream_body["model"], "claude-sonnet-4.6");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_blocked_explicit_model_is_denied_before_upstream() {
    let request_payload = json!({
        "model": "claude-sonnet-4.6",
        "messages": [
            {
                "role": "user",
                "content": "This should be denied."
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-blocked-model-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "claude-sonnet-4.6",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "should not be called"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_like_nonstream_runtime_case_with_options(
        &OPENAI_CASE,
        request_payload,
        response_payload,
        &[],
        None,
        Some(vec!["claude-sonnet-4.6".to_string()]),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::BAD_REQUEST, "response body: {body}");
    assert!(
        body.contains("model_access_denied")
            || body.contains("ModelAccessDenied")
            || body.contains("not allowed"),
        "expected virtual-key model deny response, got: {body}"
    );
    assert!(
        requests.is_empty(),
        "blocked request should not reach upstream"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_async_openai_claude_dash_model_is_normalized_before_upstream() {
    let request_payload = json!({
        "model": "claude-sonnet-4-6",
        "messages": [
            {
                "role": "user",
                "content": "Hello, normalized model."
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-normalized-model-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "claude-sonnet-4.6",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "ok"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 4,
            "completion_tokens": 1,
            "total_tokens": 5
        }
    });

    let (status, body, requests) = run_openai_like_nonstream_runtime_case_with_options(
        &OPENAI_CASE,
        request_payload,
        response_payload,
        &[("user-agent", "AsyncOpenAI/0.29.0")],
        None,
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream body should be valid json");
    assert_eq!(upstream_body["model"], "claude-sonnet-4.6");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_unified_api_multimodal_request() {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-openai-image-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "It looks like a test image."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 14,
            "completion_tokens": 8,
            "total_tokens": 22
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OPENAI_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o-mini");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
}

/// Test that requests are properly passed through to the Anthropic provider
/// when using the /ai base url and using an anthropic model in the `model`
/// field.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_unified_api() {
    let (status, body) = run_unified_case(&ANTHROPIC_CASE)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_anthropic_openai_chat_completion_shape(&body);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_unified_api_stream() {
    let request_payload = json!({
        "model": ANTHROPIC_CASE.model,
        "stream": true,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });

    let (headers, body, requests) = run_anthropic_stream_case(request_payload)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(
        body.starts_with("data: "),
        "stream response should use SSE data prefix: {body}"
    );
    assert!(
        body.contains("\"object\":\"chat.completion.chunk\""),
        "stream response should contain chat.completion.chunk payload: {body}"
    );
    assert!(
        body.contains("\"content\":\"Hello\""),
        "stream response should contain streamed text delta: {body}"
    );
    assert!(
        body.contains("\"finish_reason\":\"stop\""),
        "stream response should end with OpenAI stop finish_reason: {body}"
    );
    assert_eq!(requests.len(), 1, "expected one upstream anthropic request");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_unified_api_tool_calls() {
    let request_payload = json!({
        "model": ANTHROPIC_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "id": "msg_tool_1",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "lookup_weather",
                "input": { "city": "Paris" }
            }
        ],
        "model": "claude-sonnet-4-0",
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 12,
            "output_tokens": 6
        }
    });

    let (status, body, requests) =
        run_anthropic_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    let arguments = payload["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("tool arguments should be a string");
    let arguments: Value = serde_json::from_str(arguments).expect("tool arguments should be json");
    assert_eq!(arguments["city"], "Paris");

    assert_eq!(requests.len(), 1, "expected one upstream anthropic request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream anthropic body should be valid json");
    assert_eq!(upstream_body["tools"][0]["name"], "lookup_weather");
    assert_eq!(
        upstream_body["tools"][0]["input_schema"]["required"][0],
        "city"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_unified_api_reasoning_effort() {
    let request_payload = json!({
        "model": ANTHROPIC_CASE.model,
        "reasoning_effort": "high",
        "messages": [
            {
                "role": "user",
                "content": "Think carefully and answer."
            }
        ]
    });
    let response_payload = json!({
        "id": "msg_reasoning_1",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": "I thought carefully."
            }
        ],
        "model": "claude-sonnet-4-0",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 16,
            "output_tokens": 10
        }
    });

    let (status, body, requests) =
        run_anthropic_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream anthropic request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream anthropic body should be valid json");
    assert_eq!(upstream_body["thinking"]["type"], "enabled");
    assert!(
        upstream_body["thinking"]["budget_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens >= 1024),
        "upstream anthropic request should carry thinking budget: \
         {upstream_body}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_unified_api_multimodal_request() {
    let request_payload = json!({
        "model": ANTHROPIC_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a1sAAAAASUVORK5CYII="
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "msg_image_1",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "text",
                "text": "It looks like a test image."
            }
        ],
        "model": "claude-sonnet-4-0",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 14,
            "output_tokens": 8
        }
    });

    let (status, body, requests) =
        run_anthropic_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream anthropic request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream anthropic body should be valid json");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][0]["text"],
        "What is in this image?"
    );
    assert_eq!(upstream_body["messages"][0]["content"][1]["type"], "image");
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["source"]["type"],
        "base64"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["source"]["data"],
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII="
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn gemini_unified_api() {
    let request_payload = json!({
        "model": GEMINI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let response_payload = json!({
        "id": "gemini-unified-test-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gemini-2.0-flash",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello from Gemini."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 6,
            "completion_tokens": 4,
            "total_tokens": 10
        }
    });

    let (status, body, requests) =
        run_gemini_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);

    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["model"], "gemini-2.0-flash");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "Hello from Gemini."
    );

    assert_eq!(requests.len(), 1, "expected one upstream gemini request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream gemini body should be valid json");
    assert_eq!(upstream_body["model"], "gemini-2.0-flash");
    assert_eq!(upstream_body["messages"][0]["content"], "Hello, world!");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn gemini_unified_api_stream() {
    let request_payload = json!({
        "model": GEMINI_CASE.model,
        "stream": true,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });

    let (headers, body, requests) = run_gemini_stream_case(request_payload)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(
        body.starts_with("data: "),
        "stream response should use SSE data prefix: {body}"
    );
    assert!(
        body.contains("\"object\":\"chat.completion.chunk\""),
        "stream response should contain chat.completion.chunk payload: {body}"
    );
    assert!(
        body.contains("\"content\":\"Hello\""),
        "stream response should contain streamed text delta: {body}"
    );
    assert!(
        !body.contains("[DONE]"),
        "gateway should consume upstream [DONE] marker instead of forwarding \
         it: {body}"
    );

    assert_eq!(requests.len(), 1, "expected one upstream gemini request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream gemini body should be valid json");
    assert_eq!(upstream_body["model"], "gemini-2.0-flash");
    assert_eq!(upstream_body["stream"], true);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn gemini_unified_api_tool_calls() {
    let request_payload = json!({
        "model": GEMINI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-gemini-tool-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gemini-2.0-flash",
        "choices": [
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_gemini_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                }
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18
        }
    });

    let (status, body, requests) =
        run_gemini_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );

    assert_eq!(requests.len(), 1, "expected one upstream gemini request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream gemini body should be valid json");
    assert_eq!(upstream_body["model"], "gemini-2.0-flash");
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(
        upstream_body["tools"][0]["function"]["parameters"]["required"][0],
        "city"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn gemini_unified_api_multimodal_request() {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": GEMINI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-gemini-image-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gemini-2.0-flash",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "It looks like a test image."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 14,
            "completion_tokens": 8,
            "total_tokens": 22
        }
    });

    let (status, body, requests) =
        run_gemini_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);

    assert_eq!(requests.len(), 1, "expected one upstream gemini request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream gemini body should be valid json");
    assert_eq!(upstream_body["model"], "gemini-2.0-flash");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][0]["text"],
        "What is in this image?"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image_url"]["url"],
        data_uri
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn ollama_unified_api() {
    let request_payload = json!({
        "model": OLLAMA_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-ollama-test-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "llama3",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello from Ollama."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "total_tokens": 12
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OLLAMA_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["model"], "llama3");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "Hello from Ollama."
    );

    assert_eq!(requests.len(), 1, "expected one upstream ollama request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream ollama body should be valid json");
    assert_eq!(upstream_body["model"], "llama3");
    assert_eq!(upstream_body["messages"][0]["content"], "Hello, world!");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn ollama_unified_api_stream() {
    let request_payload = json!({
        "model": OLLAMA_CASE.model,
        "stream": true,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });

    let (headers, body, requests) = run_openai_like_stream_case(&OLLAMA_CASE, request_payload)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    assert!(!body.contains("[DONE]"));
    assert_eq!(requests.len(), 1, "expected one upstream ollama request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream ollama body should be valid json");
    assert_eq!(upstream_body["model"], "llama3");
    assert_eq!(upstream_body["stream"], true);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn ollama_unified_api_tool_calls() {
    let request_payload = json!({
        "model": OLLAMA_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-ollama-tool-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "llama3",
        "choices": [
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_ollama_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                }
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OLLAMA_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(requests.len(), 1, "expected one upstream ollama request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream ollama body should be valid json");
    assert_eq!(upstream_body["model"], "llama3");
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn ollama_unified_api_multimodal_request() {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": OLLAMA_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-ollama-image-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "llama3",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "It looks like a test image."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 14,
            "completion_tokens": 8,
            "total_tokens": 22
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&OLLAMA_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    assert_eq!(requests.len(), 1, "expected one upstream ollama request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream ollama body should be valid json");
    assert_eq!(upstream_body["model"], "llama3");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn bedrock_unified_api() {
    let request_payload = json!({
        "model": BEDROCK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let response_payload = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "text": "Hello from Bedrock."
                    }
                ]
            }
        },
        "stopReason": "end_turn",
        "usage": {
            "inputTokens": 10,
            "outputTokens": 4,
            "totalTokens": 14
        }
    });

    let (status, body, requests) =
        run_bedrock_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["message"]["role"], "assistant");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "Hello from Bedrock."
    );
    assert_eq!(payload["choices"][0]["finish_reason"], "stop");
    assert_eq!(requests.len(), 1, "expected one upstream bedrock request");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn bedrock_unified_api_tool_calls() {
    let request_payload = json!({
        "model": BEDROCK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "toolUse": {
                            "toolUseId": "toolu_bedrock_1",
                            "name": "lookup_weather",
                            "input": { "city": "Paris" }
                        }
                    }
                ]
            }
        },
        "stopReason": "tool_use",
        "usage": {
            "inputTokens": 12,
            "outputTokens": 6,
            "totalTokens": 18
        }
    });

    let (status, body, requests) =
        run_bedrock_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );

    assert_eq!(requests.len(), 1, "expected one upstream bedrock request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream bedrock body should be valid json");
    assert_eq!(
        upstream_body["toolConfig"]["tools"][0]["toolSpec"]["name"],
        "lookup_weather"
    );
    assert_eq!(
        upstream_body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"],
        Value::Null
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn bedrock_unified_api_reasoning_effort() {
    let request_payload = json!({
        "model": BEDROCK_CASE.model,
        "reasoning_effort": "high",
        "messages": [
            {
                "role": "user",
                "content": "Think carefully and answer."
            }
        ]
    });
    let response_payload = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "text": "I thought carefully on Bedrock."
                    }
                ]
            }
        },
        "stopReason": "end_turn",
        "usage": {
            "inputTokens": 16,
            "outputTokens": 10,
            "totalTokens": 26
        }
    });

    let (status, body, requests) =
        run_bedrock_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream bedrock request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream bedrock body should be valid json");
    assert_eq!(
        upstream_body["additionalModelRequestFields"]["object"]["thinking"]["object"]["type"]["string"],
        "enabled",
        "upstream bedrock body: {upstream_body}"
    );
    assert!(
        upstream_body["additionalModelRequestFields"]["object"]["thinking"]
            ["object"]["budget_tokens"]["number"]["PosInt"]
            .as_u64()
            .is_some_and(|tokens| tokens >= 1024),
        "upstream bedrock request should carry thinking budget: \
         {upstream_body}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn bedrock_unified_api_multimodal_request() {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": BEDROCK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "text": "It looks like a test image."
                    }
                ]
            }
        },
        "stopReason": "end_turn",
        "usage": {
            "inputTokens": 14,
            "outputTokens": 8,
            "totalTokens": 22
        }
    });

    let (status, body, requests) =
        run_bedrock_nonstream_runtime_case(request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "It looks like a test image."
    );
    assert_eq!(payload["choices"][0]["finish_reason"], "stop");
    assert_eq!(requests.len(), 1, "expected one upstream bedrock request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream bedrock body should be valid json");
    assert_eq!(
        upstream_body["messages"][0]["content"][0]["text"],
        "What is in this image?"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image"]["format"],
        "png"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image"]["source"]["bytes"]["inner"][0],
        137
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image"]["source"]["bytes"]["inner"][1],
        80
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image"]["source"]["bytes"]["inner"][2],
        78
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image"]["source"]["bytes"]["inner"][3],
        71
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_unified_api() {
    let request_payload = json!({
        "model": DEEPSEEK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-deepseek-test-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "deepseek-chat",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello from DeepSeek."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "total_tokens": 12
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&DEEPSEEK_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["model"], "deepseek-chat");
    assert_eq!(
        payload["choices"][0]["message"]["content"],
        "Hello from DeepSeek."
    );

    assert_eq!(requests.len(), 1, "expected one upstream deepseek request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream deepseek body should be valid json");
    assert_eq!(upstream_body["model"], "deepseek-chat");
    assert_eq!(upstream_body["messages"][0]["content"], "Hello, world!");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_reasoner_unified_api_preserves_reasoning_effort() {
    let request_payload = json!({
        "model": DEEPSEEK_REASONER_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "hello"
            }
        ],
        "reasoning_effort": "high"
    });
    let response_payload: Value = serde_json::from_str(include_str!(
        "fixtures/mapper-profiles/deepseek/deepseek-reasoner/basic-response.\
         json"
    ))
    .expect("fixture should be valid json");

    let (status, body, requests) = run_openai_like_nonstream_runtime_case(
        &DEEPSEEK_REASONER_CASE,
        request_payload,
        response_payload,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["model"], "deepseek-reasoner");
    assert_eq!(requests.len(), 1, "expected one upstream deepseek request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream deepseek body should be valid json");
    assert_eq!(upstream_body["model"], "deepseek-reasoner");
    assert_eq!(upstream_body["reasoning_effort"], json!("high"));
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_unified_api_stream() {
    let request_payload = json!({
        "model": DEEPSEEK_CASE.model,
        "stream": true,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });

    let (headers, body, requests) = run_openai_like_stream_case(&DEEPSEEK_CASE, request_payload)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8"),
        "response body: {body}"
    );
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    assert!(!body.contains("[DONE]"));
    assert_eq!(requests.len(), 1, "expected one upstream deepseek request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream deepseek body should be valid json");
    assert_eq!(upstream_body["model"], "deepseek-chat");
    assert_eq!(upstream_body["stream"], true);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_unified_api_tool_calls() {
    let request_payload = json!({
        "model": DEEPSEEK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "What is the weather in Paris?"
            }
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup_weather",
                    "description": "Look up current weather.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-deepseek-tool-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "deepseek-chat",
        "choices": [
            {
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_deepseek_weather_1",
                            "type": "function",
                            "function": {
                                "name": "lookup_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        }
                    ]
                }
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 6,
            "total_tokens": 18
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&DEEPSEEK_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    let payload: Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "lookup_weather"
    );
    assert_eq!(requests.len(), 1, "expected one upstream deepseek request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream deepseek body should be valid json");
    assert_eq!(upstream_body["model"], "deepseek-chat");
    assert_eq!(
        upstream_body["tools"][0]["function"]["name"],
        "lookup_weather"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_unified_api_multimodal_request() {
    let data_uri = "data:image/png;base64,\
         iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/\
         x8AAwMCAO+/a1sAAAAASUVORK5CYII=";
    let request_payload = json!({
        "model": DEEPSEEK_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "What is in this image?"
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-deepseek-image-1",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "deepseek-chat",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "It looks like a test image."
                }
            }
        ],
        "usage": {
            "prompt_tokens": 14,
            "completion_tokens": 8,
            "total_tokens": 22
        }
    });

    let (status, body, requests) =
        run_openai_like_nonstream_runtime_case(&DEEPSEEK_CASE, request_payload, response_payload)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
    assert_eq!(requests.len(), 1, "expected one upstream deepseek request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream deepseek body should be valid json");
    assert_eq!(upstream_body["model"], "deepseek-chat");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_without_handler_passthroughs_body() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "passthrough request"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-no-handler",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "passthrough ok"
                }
            }
        ]
    });

    let (status, body, requests) =
        run_openai_large_context_runtime_case(request_payload.clone(), response_payload, &[], &[])
            .await
            .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o-mini");
    assert_eq!(
        upstream_body["messages"][0]["content"],
        request_payload["messages"][0]["content"]
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_invalid_handler_returns_bad_request() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-invalid-handler",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "unexpected success"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "explode")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::BAD_REQUEST, "response body: {body}");
    assert!(
        body.contains("Invalid large context handler"),
        "response body: {body}"
    );
    assert!(
        requests.is_empty(),
        "unexpected upstream request body: {requests:?}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_missing_model_returns_bad_request() {
    let request_payload = json!({
        "messages": [
            {
                "role": "user",
                "content": "missing model"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-missing-model",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "unexpected success"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "truncate")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::BAD_REQUEST, "response body: {body}");
    assert!(
        body.contains("missing field `model`") || body.contains("missing field \\\"model\\\""),
        "response body: {body}"
    );
    assert!(
        requests.is_empty(),
        "unexpected upstream request body: {requests:?}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_ignores_legacy_override_headers() {
    let request_payload = json!({
        "messages": [
            {
                "role": "user",
                "content": large_context_text(600_000)
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-legacy-override",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "legacy fallback should be ignored"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[
            ("Legacy-Token-Limit-Exception-Handler", "fallback"),
            ("Legacy-Model-Override", "openai/gpt-4o-mini,openai/gpt-4o"),
        ],
        &["gpt-4o"],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::BAD_REQUEST, "response body: {body}");
    assert!(
        body.contains("missing field `model`") || body.contains("missing field \\\"model\\\""),
        "response body: {body}"
    );
    assert!(
        requests.is_empty(),
        "unexpected upstream request body: {requests:?}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_fallback_over_limit_switches_model() {
    let long_content = large_context_text(600_000);
    let request_payload = json!({
        "model": "openai/gpt-4o-mini,openai/gpt-4o",
        "messages": [
            {
                "role": "user",
                "content": long_content
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-fallback",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "fallback ok"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 120000,
            "completion_tokens": 4,
            "total_tokens": 120004
        }
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "fallback")],
        &["gpt-4o"],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_fallback_over_limit_emits_log_metadata() {
    let request_payload = json!({
        "model": "openai/gpt-4o-mini,openai/gpt-4o",
        "messages": [
            {
                "role": "user",
                "content": large_context_text(600_000)
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-fallback-log",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "fallback log ok"
                }
            }
        ]
    });

    let (status, body, upstream_requests, log_requests) =
        run_openai_large_context_runtime_case_with_logs(
            request_payload,
            response_payload,
            &[("Alephant-Token-Limit-Exception-Handler", "fallback")],
            &["gpt-4o"],
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        upstream_requests.len(),
        1,
        "expected one upstream openai request"
    );
    assert_eq!(log_requests.len(), 1, "expected one alephant log request");

    let log_body: Value = log_requests[0]
        .body_json()
        .expect("alephant log body should be valid json");
    assert_eq!(log_body["alephantMeta"]["largeContextHandler"], "fallback");
    assert_eq!(
        log_body["alephantMeta"]["largeContextAction"],
        "fallback-applied"
    );
    assert_eq!(
        log_body["alephantMeta"]["largeContextOriginalModel"],
        "openai/gpt-4o-mini,openai/gpt-4o"
    );
    assert_eq!(
        log_body["alephantMeta"]["largeContextEffectiveModel"],
        "openai/gpt-4o"
    );
    assert!(
        log_body["alephantMeta"]["largeContextEstimatedTokens"]
            .as_u64()
            .is_some(),
        "alephant log body: {log_body}"
    );
    assert!(
        log_body["alephantMeta"]["largeContextInputBudgetTokens"]
            .as_u64()
            .is_some(),
        "alephant log body: {log_body}"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_strips_session_headers_before_upstream() {
    let request_payload = json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "track unified api session"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-session-unified-strip",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "session ok"
                }
            }
        ]
    });

    let (status, body, upstream_requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[
            ("Alephant-Session-Id", "session-123"),
            ("Alephant-Session-Path", "workflow/step-1"),
            ("Alephant-Session-Name", "Planner"),
        ],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        upstream_requests.len(),
        1,
        "expected one upstream openai request"
    );

    let upstream_headers = &upstream_requests[0].headers;
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-id")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-path")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-name")
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_writes_alephant_session_headers_to_logs() {
    let request_payload = json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "prefer alephant session headers"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-session-unified-log",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "session log ok"
                }
            }
        ]
    });

    let (status, body, upstream_requests, log_requests) =
        run_openai_large_context_runtime_case_with_logs(
            request_payload,
            response_payload,
            &[
                ("Alephant-Session-Id", "preferred-session"),
                ("Alephant-Session-Path", "workflow/step-1"),
                ("Alephant-Session-Name", "Preferred Planner"),
            ],
            &[],
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(
        upstream_requests.len(),
        1,
        "expected one upstream openai request"
    );
    assert_eq!(log_requests.len(), 1, "expected one alephant log request");

    let upstream_headers = &upstream_requests[0].headers;
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-id")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-path")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-name")
    );

    let log_body: Value = log_requests[0]
        .body_json()
        .expect("alephant log body should be valid json");
    assert_eq!(
        log_body["log"]["request"]["properties"]["Alephant-Session-Id"],
        "preferred-session"
    );
    assert_eq!(log_body["log"]["request"]["sessionId"], "preferred-session");
    assert_eq!(
        log_body["log"]["request"]["properties"]["Alephant-Session-Path"],
        "/workflow/step-1"
    );
    assert_eq!(
        log_body["log"]["request"]["properties"]["Alephant-Session-Name"],
        "Preferred Planner"
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_fallback_without_second_candidate_keeps_model() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": large_context_text(600_000)
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-fallback-single-candidate",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "single candidate ok"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "fallback")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o-mini");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_fallback_below_limit_keeps_primary_model() {
    let request_payload = json!({
        "model": "openai/gpt-4o-mini,openai/gpt-4o",
        "messages": [
            {
                "role": "user",
                "content": "short request"
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-fallback-primary",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "primary ok"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "fallback")],
        &["gpt-4o"],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["model"], "gpt-4o-mini");
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_truncate_over_limit_shrinks_content() {
    let long_content = large_context_text(600_000);
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": long_content
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-truncate",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "truncate ok"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 100000,
            "completion_tokens": 4,
            "total_tokens": 100004
        }
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "truncate")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    let upstream_content = upstream_body["messages"][0]["content"]
        .as_str()
        .expect("upstream content should be text");
    assert!(
        upstream_content.len() < 600_000,
        "upstream content should be truncated, got {} chars",
        upstream_content.len()
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_truncate_skips_non_text_messages() {
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": large_context_text(50_000)
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/test.png"
                        }
                    }
                ]
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-non-text",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "skip ok"
                }
            }
        ]
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload.clone(),
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "truncate")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    assert_eq!(upstream_body["messages"][0]["role"], "user");
    assert_eq!(upstream_body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(
        upstream_body["messages"][0]["content"][0]["text"],
        request_payload["messages"][0]["content"][0]["text"]
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["type"],
        "image_url"
    );
    assert_eq!(
        upstream_body["messages"][0]["content"][1]["image_url"]["url"],
        request_payload["messages"][0]["content"][1]["image_url"]["url"]
    );
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn unified_api_large_context_middle_out_over_limit_preserves_edges() {
    let long_content = format!("HEAD:{}:TAIL", large_context_text(600_000));
    let request_payload = json!({
        "model": OPENAI_CASE.model,
        "messages": [
            {
                "role": "user",
                "content": long_content
            }
        ]
    });
    let response_payload = json!({
        "id": "chatcmpl-large-context-middle-out",
        "object": "chat.completion",
        "created": 1748543700,
        "model": "gpt-4o-mini",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "middle out ok"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 100000,
            "completion_tokens": 4,
            "total_tokens": 100004
        }
    });

    let (status, body, requests) = run_openai_large_context_runtime_case(
        request_payload,
        response_payload,
        &[("Alephant-Token-Limit-Exception-Handler", "middle-out")],
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_eq!(requests.len(), 1, "expected one upstream openai request");
    let upstream_body: Value = requests[0]
        .body_json()
        .expect("upstream openai body should be valid json");
    let upstream_content = upstream_body["messages"][0]["content"]
        .as_str()
        .expect("upstream content should be text");
    assert!(
        upstream_content.len() < 600_010,
        "upstream content should be shortened, got {} chars",
        upstream_content.len()
    );
    assert!(
        upstream_content.starts_with("HEAD:"),
        "middle-out should preserve the start: {upstream_content}"
    );
    assert!(
        upstream_content.ends_with(":TAIL"),
        "middle-out should preserve the end: {upstream_content}"
    );
}
openai_compatible_named_provider_suite!(
    qwen_unified_api,
    qwen_unified_api_stream,
    qwen_unified_api_tool_calls,
    qwen_unified_api_multimodal_request,
    qwen_unified_api_capability_fields_passthrough,
    QWEN_CASE
);

openai_compatible_named_provider_suite!(
    minimax_unified_api,
    minimax_unified_api_stream,
    minimax_unified_api_tool_calls,
    minimax_unified_api_multimodal_request,
    minimax_unified_api_capability_fields_passthrough,
    MINIMAX_CASE
);

openai_compatible_named_provider_suite!(
    moonshotai_unified_api,
    moonshotai_unified_api_stream,
    moonshotai_unified_api_tool_calls,
    moonshotai_unified_api_multimodal_request,
    moonshotai_unified_api_capability_fields_passthrough,
    MOONSHOTAI_CASE
);

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn mistral_unified_api_non_phase1() {
    assert_unified_case_ok(&MISTRAL_CASE).await;
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn mistral_unified_api_stream() {
    assert_openai_like_stream_case_ok(&MISTRAL_CASE).await;
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn mistral_unified_api_tool_calls() {
    assert_openai_like_tool_calls_case_ok(&MISTRAL_CASE).await;
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn mistral_unified_api_multimodal_request() {
    assert_openai_like_multimodal_case_ok(&MISTRAL_CASE).await;
}

openai_compatible_named_provider_suite!(
    groq_unified_api,
    groq_unified_api_stream,
    groq_unified_api_tool_calls,
    groq_unified_api_multimodal_request,
    groq_unified_api_capability_fields_passthrough,
    GROQ_CASE
);

openai_compatible_named_provider_suite!(
    xai_unified_api,
    xai_unified_api_stream,
    xai_unified_api_tool_calls,
    xai_unified_api_multimodal_request,
    xai_unified_api_capability_fields_passthrough,
    XAI_CASE
);

openai_compatible_named_provider_suite!(
    hyperbolic_unified_api,
    hyperbolic_unified_api_stream,
    hyperbolic_unified_api_tool_calls,
    hyperbolic_unified_api_multimodal_request,
    hyperbolic_unified_api_capability_fields_passthrough,
    HYPERBOLIC_CASE
);
