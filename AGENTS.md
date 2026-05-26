# AGENTS.md

> **系统指令 (System Prompt)**
>
> 本文件是本项目（及所在团队）的最高架构与开发指导原则。
>
> **适用对象**: Cursor, Augment, Kilo Code, Claude Code, OpenCode 等 AI 编程助手。
>
> **核心原则**: 在阅读代码、生成新功能或进行重构时，**必须优先遵循**本文档中的架构约定与代码规范。
>
> **注意**: 忽略本文档规范生成的代码，将无法通过 CI/CD 静态扫描或构建检查。

---

## 0. ⚡️ AI 快速上下文 (AI Quick Context)

### 0.1 业务背景与核心职责 (Business Context)

- **服务定位**: `Alephant AI Gateway` 是一个高性能 LLM 代理路由网关，提供 OpenAI 兼容 API，支持 50+ 提供商、320+ 模型。
- **用户与场景 (User Personas & Scenarios)**:
  - **开发者 (Developer)**:
    - **场景**: 通过统一 API 接入多个 AI 提供商（如 OpenAI、Anthropic、AWS Bedrock），无需重复集成。
    - **痛点**: 各提供商 API 格式不一，需要适配；需要路由、缓存、重试、熔断等治理能力。
  - **平台团队 (Platform Team)**:
    - **场景**: 管理 API Key、配置路由策略、监控用量、实施访问控制。
    - **痛点**: 需要统一观测和审计每个请求。
  - **AI Agent**: Cursor、Codex、opencode 等 Agent 客户端通过统一 API 接入。
- **绝对边界 (What We Don't Do)**:
  - ❌ **不提供** LLM 模型本身，只做路由和适配。
  - ❌ **不存储** 原始用户数据（仅存储必要的路由元数据）。
  - ❌ **不处理** 业务逻辑，只做 AI 流量的透明代理。

### 0.2 📋 项目详细功能需求说明 (Detailed Functional Specs)

#### 1. 核心路由能力
- **多提供商支持**: 支持 50+ AI 提供商，包括 OpenAI、Anthropic、AWS Bedrock、Azure、Google Vertex 等。
- **模型路由**: 支持动态路由、延迟路由、加权负载均衡。
- **故障恢复**: 支持 fallback、retries、health check、熔断机制。
- **错误处理**: 统一错误格式，错误适配（provider 错误 -> 标准错误）。

#### 2. 请求适配与标准化
- **请求适配**: 请求转换（tool、streaming、error、usage、finish-reason）。
- **响应标准化**: 统一响应格式为 OpenAI 兼容格式。
- **缓存**: LLM KV Cache、Semantic Cache。

#### 3. 安全与访问控制
- **Key 管理**: Virtual Keys、Master Key 解析、Key Hash。
- **策略控制**: Model Policy、Workspace Provider Allowlist、并发控制。
- **隔离**: Provider 隔离、请求隔离。

#### 4. 可观测性
- **日志**: 请求日志、Trace、Body Archival。
- **指标**: Prometheus Metrics。
- **监控**: 健康监控、限流监控。

### 0.3 📝 领域术语表 (Domain Glossary)

| 中文业务词 | 英文术语 (Code/DB) | 备注 |
| :--- | :--- | :--- |
| AI 网关 | AI Gateway | 核心服务 |
| 提供商 | Provider | AI 服务提供商 |
| 模型 | Model | LLM 模型 |
| 虚拟 Key | Virtual Key | 用户 API Key |
| 主 Key | Master Key | 主密钥 |
| 路由策略 | Router Policy | 路由规则 |
| Fallback | Fallback | 故障转移 |
| 重试 | Retry | 自动重试 |
| 熔断 | Circuit Breaker | 故障熔断 |
| 缓存 | Cache | LLM KV / Semantic |
| 语义缓存 | Semantic Cache | 相似请求缓存 |
| 健康监控 | Health Monitor | Provider 健康检查 |
| 限流监控 | Rate Limit Monitor | 限流状态监控 |

### 0.4 🗺 功能能力与 API 映射 (Functional Capability & API Map)

#### 1. 核心路由 (Routing)
- **代码位置**: `ai-gateway/src/router/`
- **关键模块**:
  - `dynamic-router/`: 动态路由
  - `latency-router/`: 基于延迟的路由
  - `weighted-balance/`: 加权负载均衡
- **路由策略**: Direct、Policy-based、Latency-based、Weighted

#### 2. Provider 适配 (Provider Adaptation)
- **代码位置**: `ai-gateway/src/discover/`
- **核心能力**: 请求/响应标准化、错误转换、Provider 健康状态管理

#### 3. 缓存层 (Caching)
- **代码位置**: `ai-gateway/src/semantic_cache/`
- **核心模块**: `alephant-llm-kv-cache` crate - LLM KV Cache
- **缓存策略**: Cache Hit/Miss、相似度匹配

#### 4. 安全与 Key 管理
- **代码位置**: `ai-gateway/src/crypto/`
- **核心能力**: Key Hash、Virtual Key 解析、加密

#### 5. 可观测性
- **代码位置**: `ai-gateway/src/metrics/`、`ai-gateway/src/logger/`
- **核心模块**: `telemetry` crate
- **指标**: HTTP 指标、Provider 指标、系统指标

#### 6. API 端点
- **OpenAI 兼容**: `/v1/chat/completions`、`/v1/completions`、`/v1/embeddings`
- **统一 API**: `/ai/*` 路由

#### 7. 安全插件系统 (Security Plugin System)
- **代码位置**: `ai-gateway/src/plugin/`
- **核心文件**:
  - `mod.rs`: [`SecurityPlugin`] trait 定义、注册表
  - `loader.rs`: [`PluginLoader`] 配置驱动加载器
  - `builtins.rs`: 内置插件 (NoOp、SensitiveDataDetector、DataClassifier)
- **插件接口**:
  - [`SecurityPlugin::check_request`]: 请求前置检查
  - [`SecurityPlugin::mask_response`]: 响应后置脱敏
- **配置示例**:
  ```yaml
  global:
    middleware:
      security:
        enabled: true
        plugins:
          - name: sensitive_data_detector
            enabled: true
            priority: 10
            config:
              fields: [password, phone, id_card, bank_account]
  ```

### 0.5 📦 仓库部署与多服务定义 (Repository & Deployment Scope)

- **仓库类型**: Rust Workspace (单仓库多 crate)
- **主要服务**: `ai-gateway` (主服务)
- **辅助 Crates**:
  1. `crates/alephant-llm-kv-cache`: LLM KV 缓存
  2. `crates/dynamic-router`: 动态路由
  3. `crates/latency-router`: 延迟路由
  4. `crates/weighted-balance`: 加权负载均衡
  5. `crates/telemetry`: 可观测性基础设施
  6. `crates/mock-server`: 测试用 Mock 服务器
  7. `crates/no-cjk-rust-comments`: 注释规范检查
  8. `crates/gateway-e2e-harness`: E2E 测试工具
- **部署模式**: Docker (self-hosted) / Fly.io (云部署)

### 0.6 关键依赖 (Critical Dependencies)

- **Web 框架**: Axum 0.8 + Tower
- **异步运行时**: Tokio
- **数据库**: PostgreSQL (sqlx)
- **缓存**: Redis + Moka
- **gRPC**: Tonic
- **HTTP Client**: Reqwest
- **可观测性**: OpenTelemetry + Tracing + Prometheus
- **配置**: Clap (CLI) + serde_yaml (配置文件)
- **加密**: rustls + ring

### 0.7 热点代码路径 (Hot Code Paths)

- 🔥 **核心路由**: `ai-gateway/src/dispatcher/` - 请求分发核心
- 🔥 **Provider 调用**: `ai-gateway/src/discover/` - Provider 调用链路
- 🔥 **缓存查找**: `ai-gateway/src/semantic_cache/` - 语义缓存匹配
- 🔥 **中间件链**: `ai-gateway/src/middleware/` - 请求处理中间件
- 🔥 **安全插件**: `ai-gateway/src/plugin/` - 插件加载与执行

### 0.8 运行时环境配置 (Runtime Environment Config)

#### 0.8.1 网关列表 (Gateway List)

> 当前项目为开源项目，无内部网关配置。

#### 0.8.2 服务信息 (Service Info)

| 可执行文件 (bin) | 类型 | 说明 |
| :--- | :--- | :--- |
| ai-gateway | 主服务 | Rust HTTP 服务，Axum + Tower |

---

## 1. 🏗 技术栈与基础设施 (Tech Stack)

- **Language**: Rust (Edition 2024)
- **Async Runtime**: Tokio
- **Web Framework**: Axum 0.8 + Tower (Middleware)
- **Database (OLTP)**: PostgreSQL (sqlx)
- **Cache**: Redis + Moka (in-memory LRU)
- **gRPC**: Tonic
- **HTTP Client**: Reqwest
- **Config**: Clap (CLI) + serde_yaml
- **Crypto**: rustls + ring
- **Observability**: OpenTelemetry + Tracing + Prometheus + Grafana + Loki + Tempo
- **Build Tool**: Cargo (Workspace)
- **CI/CD**: GitHub Actions, Cargo Husky (pre-commit hooks)

---

## 2. 📂 目录结构与架构职责 (Directory Map)

```
Project_Root (Cargo Workspace)
├── AGENTS.md                        # AI 开发指南 (本文件)
├── Cargo.toml                       # Workspace 根配置
├── Cargo.lock                       # 依赖锁定
├── ai-gateway/                      # 🚀 主服务
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                  # 入口 (Meltdown 服务编排)
│       ├── lib.rs
│       ├── app.rs                   # App 核心
│       ├── app_state.rs             # 应用状态
│       ├── app_redis.rs             # Redis 客户端
│       ├── config/                  # 配置定义
│       │   └── security_plugin.rs   # 安全插件配置
│       ├── endpoints/               # HTTP 端点定义
│       ├── router/                  # 路由策略实现
│       ├── discover/                # Provider 发现与调用
│       │   ├── monitor/             # 健康监控、限流监控
│       │   └── provider.rs
│       ├── dispatcher/              # 请求分发
│       ├── middleware/               # Tower Middleware
│       │   └── security.rs          # 安全插件中间件
│       ├── plugin/                  # 🔌 安全插件系统
│       │   ├── mod.rs               # SecurityPlugin trait、注册表
│       │   ├── loader.rs            # PluginLoader 配置驱动加载
│       │   └── builtins.rs          # 内置插件实现
│       ├── metrics/                  # Prometheus 指标
│       ├── logger/                  # Tracing 日志
│       ├── store/                   # 数据存储 (DB Listener)
│       ├── error/                   # 错误类型定义
│       ├── types/                   # 核心类型定义
│       ├── content_filter/          # 内容过滤
│       ├── crypto/                  # 加密模块
│       ├── fallback/                # Fallback 策略
│       ├── semantic_cache/          # 语义缓存
│       ├── virtual_key/            # 虚拟 Key
│       ├── default_model/           # 默认模型
│       └── proto/                   # Protobuf 定义
├── crates/                          # 共享 Crates
│   ├── alephant-llm-kv-cache/      # LLM KV 缓存
│   ├── dynamic-router/              # 动态路由
│   ├── latency-router/              # 延迟路由
│   ├── weighted-balance/           # 加权负载均衡
│   ├── telemetry/                  # 可观测性基础设施
│   ├── mock-server/                # Mock 服务器
│   ├── no-cjk-rust-comments/       # 注释规范检查
│   └── gateway-e2e-harness/         # E2E 测试工具
├── infrastructure/                  # 基础设施配置
│   ├── compose.yaml                 # Docker Compose
│   ├── prometheus/                  # Prometheus 配置
│   ├── grafana/                     # Grafana 配置
│   ├── loki/                        # Loki 配置
│   ├── tempo/                       # Tempo 配置
│   ├── redis/                       # Redis 配置
│   └── terraform/                   # 基础设施代码
├── scripts/                         # 工具脚本
│   ├── test/                        # 测试工具
│   └── provider_mock/               # Provider Mock
├── vendor/                          # Patched 依赖
├── examples/                       # 示例
├── test-artifacts/                 # 测试资源
├── Makefile                        # 构建脚本
└── README.md                       # 项目文档
```

### 数据流向 (Data Flow)

```
Request (HTTP/gRPC)
  -> Middleware (Auth, Rate Limit, Logging)
  -> Endpoint (Router Handler)
  -> Dispatcher (Route Policy)
  -> Provider Adapter (Request Transform)
  -> Upstream Provider (OpenAI, Anthropic, etc.)
  -> Provider Adapter (Response Transform)
  -> Cache Check (Optional)
  -> Response
```

---

## 3. 🛠 开发规范与最佳实践 (Development Guidelines)

### 3.1 强制构建与运行流程 (Mandatory Workflows)

**AI 在生成执行指令或修改依赖时，必须严格遵守以下流程：**

#### 1. 依赖管理流程
```bash
cargo build
```
- **适用场景**: 首次构建或依赖变更后。

#### 2. 代码检查流程
```bash
cargo clippy --all-targets --all-features
cargo fmt --check
cargo test --all-features
```
- **适用场景**: 提交前必须通过所有检查。

#### 3. 构建发布流程
```bash
cargo build --release
cargo test --all-features -- --ignored  # 运行集成测试
```
- **适用场景**: 生产环境构建。

#### 4. 本地运行流程
```bash
# 方式1: 直接运行
cargo run --bin ai-gateway

# 方式2: 指定配置
RUST_LOG=debug cargo run --bin ai-gateway -- --config config.yaml
```

### 3.2 代码修改边界 (Code Boundaries)

✅ **可修改区域**：
- `ai-gateway/src/` - 主服务业务代码
- `crates/*/` - 各 crate 源代码
- `infrastructure/` - 基础设施配置
- `scripts/` - 工具脚本
- `Cargo.toml` - 依赖声明
- `config/` - 配置文件
- `docs/` - 文档
- **`ai-gateway/src/plugin/` - 安全插件（可扩展区域）**

❌ **禁止修改区域**：
- `vendor/` - Patched 依赖（自动管理）
- `target/` - 编译输出目录
- `Cargo.lock` - 依赖锁定（自动生成）

### 3.3 Rust 特定规范

#### Error Handling
- 使用 `thiserror` 定义错误类型
- 使用 `anyhow` 进行上下文包装
- **禁止**: `unwrap()`, `expect()` 在生产代码中
- **必须**: `?` 操作符进行错误传播

#### Async/Await
- 使用 `#[tokio::main]` 或 `#[tokio::test]`
- 使用 `async-trait` 处理 trait 的 async 方法
- **禁止**: 在同步上下文中使用 `.await`

#### Trait Bounds
- 使用 `?Sized` 处理动态大小类型
- 使用 `Send + Sync` 确保线程安全
- **注意**: `Rc<T>` 不是 `Send` 或 `Sync`

### 3.4 数据库操作规范 (Database Rules)

- **ORM**: 使用 `sqlx` (raw SQL + compile-time checked)
- **连接池**: 使用 `sqlx::PgPool`
- **事务**: 使用 `sqlx::Transaction`
- **迁移**: 使用 `sqlx-cli` 管理 migrations
- ❌ **禁止**: ORM AutoMigrate（参见 ADR）

### 3.5 最佳代码范例 (Few-Shot Example)

**场景：编写一个异步的 Provider 调用**

```rust
// ai-gateway/src/discover/provider.rs

use async_trait::async_trait;
use thiserror::Error;
use reqwest::Client;
use tracing::{info, error};

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("request failed: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("provider returned error: {0}")]
    ApiError(String),

    #[error("timeout")]
    Timeout,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<Response, ProviderError>;
}

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn complete(&self, request: ChatCompletionRequest) -> Result<Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);

        let response = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!(%status, body, "provider API error");
            return Err(ProviderError::ApiError(format!("status: {}, body: {}", status, body)));
        }

        let response: Response = response.json().await?;
        info!(usage = ?response.usage, "completion successful");

        Ok(response)
    }
}
```

**场景 2：编写一个安全插件**

```rust
// ai-gateway/src/plugin/builtins.rs

use ai_gateway::plugin::{
    SecurityPlugin, SecurityContext, ResponseData, SecurityError, SensitivityLevel,
};

pub struct SensitiveDataDetector {
    config: SensitiveDataDetectorConfig,
}

impl SecurityPlugin for SensitiveDataDetector {
    fn name(&self) -> &'static str {
        "sensitive_data_detector"
    }

    fn priority(&self) -> i32 {
        10 // High priority - runs early
    }

    fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError> {
        // 检测敏感字段
        for field in &self.config.fields {
            if contains_sensitive_data(&ctx.request_body, field) {
                return Err(SecurityError::SensitiveDataDetected(field.clone()));
            }
        }
        Ok(())
    }

    fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError> {
        mask_sensitive_json(&mut data.body, &self.config.fields);
        Ok(())
    }
}
```

---

## 4. 🧪 测试与故障排查 (Testing & Troubleshooting)

### 4.1 测试策略

- **单元测试**: 每个 crate 独立测试，覆盖率目标 ≥ 80%
- **集成测试**: 使用 `stubr` Mock HTTP 服务
- **E2E 测试**: `gateway-e2e-harness` crate
- **测试命令**:
  ```bash
  cargo test --all-features
  cargo test --test auth --features "external integration"
  ```

### 4.2 常见故障排查

- **编译错误 `cannot find type`**:
  - 🛠 解决: 检查 `Cargo.toml` 是否正确引入依赖

- **Async Runtime 错误 `future cannot be sent between threads`**:
  - 🛠 解决: 检查 `Send + Sync` bounds，移除不必要的 `Rc`

- **数据库连接错误**:
  - 🛠 解决: 检查 `DATABASE_URL` 环境变量，确认 PostgreSQL 运行

- **Redis 连接错误**:
  - 🛠 解决: 检查 `REDIS_URL` 环境变量，确认 Redis 运行

---

## 5. 🚀 性能基准与 SLA (Performance Baselines)

<!-- TODO: 根据实际监控数据调整 SLA 值 -->

| 接口类型 | P95 延迟 | P99 延迟 | QPS 峰值 |
| --- | --- | --- | --- |
| **Chat Completion** | 500ms | 1000ms | 5000 |
| **Embedding** | 100ms | 200ms | 10000 |
| **Cache Hit** | 10ms | 20ms | 50000 |

### 5.1 已知性能瓶颈与优化建议

- **瓶颈**: Provider 上游延迟是主要因素
- **优化**: 启用 Semantic Cache 减少重复请求
- **优化**: 使用 LLM KV Cache 加速 token 生成
- **建议**: 批量请求时使用 connection pooling

---

## 6. 📜 架构决策记录 (ADRs) & 避坑指南

### 6.1 架构决策

- **ADR-001: 使用 Axum + Tower 而非 Actix-web**
  - **原因**: Tower 的 middleware 生态系统更成熟，职责分离更清晰
  - **参考**: [Tower 官方文档](https://tower.rs/)

- **ADR-002: 使用 sqlx 而非 Diesel**
  - **原因**: sqlx 支持 compile-time query checking，更安全
  - **参考**: [sqlx 文档](https://docs.rs/sqlx/)

- **ADR-003: 禁用 ORM AutoMigrate**
  - **原因**: 曾导致生产环境意外修改表结构
  - **规定**: 必须手写 Migration 脚本，使用 `sqlx-cli`

- **ADR-004: 使用 Rust Edition 2024**
  - **原因**: 支持更现代的 Rust 语法和 async traits

- **ADR-005: 安全插件系统架构**
  - **决定**: 引入 `SecurityPlugin` trait + `PluginLoader` 配置驱动架构
  - **原因**:
    - 安全需求差异化：企业版需要敏感数据检测、金融版需要 PCI-DSS 合规
    - 零侵入：插件作为独立 crate，通过配置组装，不影响主仓库逻辑
    - 可测试：每个插件独立测试，通过配置组合
  - **参考**: `ai-gateway/src/plugin/`

### 6.2 避坑指南 (Known Issues)

1. **Rust Edition**: 必须使用 Rust 2024 edition
2. **jemalloc**: 生产环境使用 jemallocator 减少内存碎片
3. **Tokio Runtime**: 必须使用 `[dependencies.tokio] features = ['full']`
4. **Async Traits**: 使用 `async-trait` crate 支持 trait 的 async 方法
5. **错误处理**: 生产代码禁止使用 `unwrap()`/`expect()`

---

## 7. 🤖 AI IDE 交互指南 (IDE Interaction)

**适用于 Cursor, Augment, Kilo Code, Claude Code, OpenCode 等工具的提示词规范：**

1. **Prompt 模板**: 生成代码时，请使用："基于 AGENTS.md 第 3.5 节代码范例，在 `ai-gateway/src/[module]` 中实现 [功能X]..."

2. **Code Review 自检**: 生成代码后，AI 需自检：
   - ❌ 禁止: `unwrap()`, `expect()`, `Rc` 在多线程上下文
   - ✅ 必须: `?` 操作符、`thiserror` 定义错误类型

3. **工具链合规**:
   - Commit Message 必须符合 Conventional Commits
   - 代码必须能通过 `cargo clippy --all-features`
   - **检查**: 如果修改了依赖关系，请主动提示运行 `cargo build`

---

## 8. 📅 文档维护与更新 (Maintenance)

> **重要**: 本文档是"活"的，必须随着项目迭代而更新。

- **更新时机**:
  1. 每次 **目录结构** 发生变更时
  2. 每次引入新的 **依赖** 或 **核心模块** 时
  3. 每次 **业务术语 (Glossary)** 发生定义变化时
  4. 每次 **Rust Edition** 或 **Tokio 版本** 升级后

- **责任人**: 项目 Tech Lead / 架构师

---

## 附录: Cloudwego eino 集成参考

本项目可考虑集成 [Cloudwego eino](https://github.com/cloudwego/eino) 作为 AI Agent 框架。

### eino 核心概念

- **Graph**: 有向无环图 (DAG)，定义 Agent 的执行流程
- **Node**: 图中的节点，代表具体执行单元 (LLM, Tool, Retriever)
- **Edges**: 节点之间的边，定义数据流向和控制流
- **Callback**: 回调机制，用于监控和调试

### 潜在集成场景

1. **复杂路由逻辑**: 使用 eino Graph 实现多阶段路由
2. **Tool Calling**: 利用 eino 的 Tool 抽象增强 Agent 能力
3. **Chain Composition**: 组合多个 Provider 形成 Chain

### 参考资料

- [eino GitHub](https://github.com/cloudwego/eino)
- [eino Examples](https://github.com/cloudwego/eino/tree/main/examples)
- [eino Docs](https://www.cloudwego.io/eino/)
