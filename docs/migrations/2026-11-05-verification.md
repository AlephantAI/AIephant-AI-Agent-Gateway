# 七项能力迁移验收记录

代码位于 `codex/alephant-seven-features`，基于目标项目 `baf26d5`。功能提交覆盖真实自托管鉴权初始化及七项能力；审查修复单独提交。原项目 main 未合并，未推送或发布远程 PR。

## 最终验证

| 验证 | 结果 |
| --- | --- |
| external 默认全部测试目标 | 1567 passed，0 failed，5 ignored |
| internal 完整库测试 | 1491 passed，0 failed，1 ignored |
| 显式 Redis 集成测试 | 4 passed：共享配额、PII 缓存、两项既有并发控制 |
| internal 生产二进制构建 | 通过 |
| 生产入口 smoke | 7 组全部通过：默认鉴权/模型/工具与业务失败、内存限流、双进程 Redis 限流、语音 SDK、图片编辑 SDK、校验、反向代理保活 |
| 格式和补丁检查 | cargo fmt --all --check、git diff --check 通过 |

external 默认跳过的 4 项 Redis 测试已单独执行；剩余 1 项是仓库原有忽略的文档示例。未启用需要额外环境的 `testing`/`integration` 特性组合，未调用付费供应商，未验证远程部署。已存在的 7 条 async-openai 弃用警告保留。

本机测试使用官方锁定提交的归档映射配置 `/private/tmp/alephant-cargo-sources.toml`；没有改变原有 Git 依赖的地址或锁定版本。新增图片依赖在 Cargo.lock 中明确记录。构建特性 external/internal 互斥，生产自托管示例使用 internal。具体命令及 smoke 选项见 [自托管说明](../../examples/self-hosted/README.md)。

## 独立审查与修复

执行过一次独立、只读的全分支审查。最初结论为不应合并；报告中的 4 项 Important 与 2 项 Minor 已全部处理，后两项按实际有效请求和状态合同的影响提升为本次必修。未进行第二轮重复审查；采用逐项 RED→GREEN 回归与完整测试验证修复。

| 问题 | 修复与验证 |
| --- | --- |
| 重复 disposition/boundary 参数造成解析差异 | 在选择参数前拒绝重复项；完整转写表单与重复模型用例先失败后通过 |
| 中文与转义文件名被拒绝 | 支持 UTF-8 quoted-string，同时保留原始头/文件字节；中文语音、图片文件名的真实 SDK 上传通过 |
| 业务失败导致工具费用被错误免除 | 独立记录业务失败，保留实际 HTTP 2xx 调用的既有成本决定；费用断言先失败后通过 |
| 校验路由将资产或业务头值写入日志 | 入口 debug、dispatcher、兜底日志及持久化视图统一保护；私有正文/头值哨兵先泄漏后不再出现，真实 usage 和原始大小保持 |
| MIME 大小写导致合法请求/响应失败 | multipart 投影与 JSON 心跳响应使用 MIME 解析比较；两个大小写回归先失败后通过 |
| 保活错误丢失真实上游状态 | 使用可信内部 transport 元数据区分 upstream_status 与 gateway_status；201→400 生产案例通过，不凭内部错误虚构上游状态 |

补跑旧的 ignored Redis 测试时发现 `incr_and_decr_refresh_ttl` 把刷新后的完整 TTL 与最初完整 TTL 比较，二者均为 3600 导致错误失败。测试改为比较刷新前已衰减的 TTL；运行时代码未变，显式重跑通过。

## 实施取舍完整记录

以下按实施顺序保留全部 Ruling，包含原因和成本。配置、功能边界见 [迁移说明](2026-11-05-self-hosted-capabilities.md)。

1. Ruling: The native create_worktree call did not return and no checkout was created; use git worktree in /private/tmp after verifying inventory. Original main stays unchanged. Cost: the app has no native worktree attachment yet.

2. Ruling: Plan headings use PR rather than Task, so maintain this ledger and use each PR section directly as its task brief. Cost: no automatic task-brief extraction.

3. Ruling: GitHub direct Git fetch timed out; official commit archives are mapped through /private/tmp/alephant-cargo-sources.toml using directory source replacement. Cargo.lock and dependency versions are unchanged; all verification commands must pass this config until normal Git checkout access is restored. Cost: verification relies on archive-sourced versions rather than a normal local git cache.

4. Ruling: authenticated self-hosting needs production local-key initialization because compat mode otherwise disables auth and Tools requires AuthContext. Add explicit local-auth with environment-only credentials and per-key provider/workspace/model binding, using the existing virtual-key middleware and provider env credentials. Cost: one additional opt-in configuration section and startup validation.

5. Ruling: the normal startup readiness gate was opened only by DatabaseListener, which does not run in compat mode. Mark readiness after local app and service-stack initialization. RED: production smoke reached model and tool success but readiness was 503; GREEN: same production smoke passed. Cost: local startup readiness now represents initialized static configuration.

6. Task 2: Ruling: runtime router exposes v1 endpoints only; legacy provider-prefixed paths mentioned in old docs are not live routes. Do not add legacy routing as part of quota work. Cost: those historical routes remain unsupported.

7. Task 3: Ruling: retain existing June 2025 initial offer, accept server-selected March/June/November 2025 for the implemented tools subset. Optional tasks/sampling/elicitation are not advertised. Cost: servers requiring a newer initial offer instead of normal negotiation will reject; supported subset is documented.

8. Task 4: Ruling: use the existing 1 MiB model-body cap for media uploads, including framing; do not silently expand the gateway resource boundary. Cost: recordings/images above this bound require a future configured upload-limit change.

9. Task 4: Ruling: keep existing virtual-key model policy (allow override, explicit block, otherwise allow); do not reinterpret allowed-models as a restrictive allowlist. Initial smoke assumption was wrong; explicit blocked-model fixture passes. Cost: operators must use the documented existing block semantics.

10. Task 4: Ruling: media endpoints use a specific raw mapper branch with existing dispatcher/auth/logs, bypass chat conversion and LLM caches, and make exactly one send without fallback. Cost: media has no cross-vendor conversion or automatic recovery; non-streaming transcription and raw speech are the supported subset.

11. Task 6: Ruling: reuse pure source validators plus strict JSON parser, pin PNG/JPEG decoder versions; add image only as dev fixture encoder. Cargo.lock adds those new packages without updating existing locked versions. Cost: decoder maintenance remains necessary; no source billing/rules runtime is copied.

12. Task 6: Ruling: multipart validation uses a temporary JSON projection of file bytes, never rewriting the outgoing upload. JSON selectors therefore also work on files at /multipart/<name>/<index>. Cost: opted-in media validation performs bounded Base64 projection allocation.

13. Task 6: Ruling: successful-response validation disables caches and requires non-streaming results; logs retain original upstream usage before returning 502. Cost: those requests cannot use response cache or streamed response checks. Header-only checks still preserve streaming.

14. Task 7: Ruling: active heartbeat paths disable all replay/reconnect/fallback and caches; bound unfinished frame, four-chunk log queue and total response/log body to 32 MiB. Cost: larger outputs fail explicitly and opt-in requests do not receive automatic recovery or caching.

15. Task 7: Ruling: heartbeat mode is selected only for configured Unified JSON model endpoints, after authentication/quota/model policy. Before commit status is preserved; afterward errors use explicit JSON/SSE envelopes and original upstream status where available. Cost: JSON clients enabling whitespace must inspect the error envelope under HTTP 200.

16. Final: Ruling: validation routes use a metadata/usage-only stored body view and suppress raw diagnostic bodies/header values from ingress through dispatcher. Original sizes, usage, status and typed request/workspace identifiers remain. Cost: raw payload debug, header-derived custom telemetry and body-derived tool observation are unavailable on these routes; plain unconfigured routes retain old logging.

17. Final: Ruling: the fresh review's Declined to judge items match explicit scope: existing exact allow/block policy; 1 MiB uploads; no media replay; no active-heartbeat caches/recovery; existing explicit MCP cached-session 404 recovery; no paid supplier or remote deployment tests. Cost: those documented limits remain.

18. Final: Ruling: raw debug collection is disabled on binary media and on JSON whitespace heartbeat paths, so an opt-in debug flag cannot bypass upload bounds or buffer away keepalives. Cost: full raw-body debug is unavailable on those paths.

## 暂缓的小项

无。本次独立审查报告的所有问题均已处理。明确范围之外的是付费供应商实测和远程发布；已裁定的上传、模型策略、协议子集及恢复限制按文档保留。
