# tests/modules/client-wpplugin — WebUI 客户端集成测桶

本桶是 `client-wpplugin` 的 **Cargo `[[test]]` 集成测**物理落点（Cargo.toml 路径映射到本目录）。箱内单测（`cargo test --lib --bins` + 前端 svelte-check + Vitest）在产品树 `client-wpplugin/source/`，不在本目录。覆盖数字以车道重跑为准（`tests/README.md` §0 快照），本页不复述第二份。

## 用例清单（8 个 `[[test]]` 目标，2026-09-25 车道实跑）

| 目标 | 用途 | 断言面 |
|------|------|--------|
| `component_template_mock_task_flow.rs` | mock 组件模板任务全流 | 组件模板从领取到回调的完整任务流（对 mock 翻译源） |
| `discoverer_endpoints.rs` | discoverer 端点交互 | `src/task_engine/discoverer.rs` 的 `wptsall/v2` 发现面 |
| `idempotency_callback.rs` | 回调幂等语义 | 每内容条目确定性 Idempotency-Key 的翻译回调行为 |
| `sign_e2e.rs` | 厂商签名算法实签验证 | 11 族 auth 算法对 mock-translate-api 实跑（baidu MD5 / youdao SHA-256 / alibaba v1 / aws SigV4 / azure / volcengine HMAC / 腾讯 TC3 / kakao / OAuth 流 / hmac / wasm-plugin；**环境依赖 :9090**） |
| `webui_route_matrix.rs` | WebUI 路由矩阵 | 数据驱动 `(method, path, payload, session-state) → (status / error-code / shape)` |
| `worker_loop.rs` | 本地 worker 环 | `pub(crate)` 入口的 claim → translate → callback 环 |
| `wp_core_source_component_flow.rs` | wp core source 组件流 | 3 测当前全 `#[ignore]`（需 live WP fixture manifest + local runtime，见文件内 `#[ignore = "requires live WP fixture manifest and local runtime"]`） |
| `wp_translation_providers_roundtrip.rs` | providers 信封往返 | 本地 HTTP 服务器回放 `wptsall-wp-providers-v1` 加密信封的 fetch 往返 |

## 执行（fail-closed 契约）

```bash
bash scripts/wptsall.sh test client-unit   # 本桶全部 [[test]] 目标 + 箱内单测
bash scripts/wptsall.sh test integration    # 亦跑 cargo --tests（含本桶）
```

- 车道从 Cargo.toml **枚举声明目标**：声明未跑 → FAIL（不静默跳过）。
- `sign_e2e` 需 mock-translate-api 于 `127.0.0.1:9090` 在跑：mock 未起 → 该目标记 **environment_missing**、车道 **exit 2**（非测试失败）。起法见 [`../../infra/mock-api/README.md`](../../infra/mock-api/README.md)。
- 其余 7 个目标自足（进程内 mock / 临时目录），无外部环境前置。

## 上层口径

lane 定位 / 矩阵行 / E2E 归属：[`../../README.md`](../../README.md) §2 · §3 · §4（本桶 UI/agent 断言主要挂 wpmmcc-ats e2e 车道——客户端是被测运行时，编排中心在 Lab/e2e）。
