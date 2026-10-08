# tests/modules/client-desktop — Desktop（Tauri）测试桶

Desktop 客户端三层测试的物理落点。覆盖数字以车道重跑为准（`tests/README.md` §0 快照），本页不复述第二份。

## 三层与入口

| 层 | 物理落点 | 入口 | 说明 |
|----|----------|------|------|
| Unit | 产品树内联（`client-desktop/src-tauri` `#[test]` + 前端 svelte-check + Vitest） | `wptsall test desktop-unit` | 不在本目录 |
| Integration | `tests/integration/run-headless-desktop.sh` | `wptsall test desktop-integration` | 无 GUI：Tauri IPC 命令与嵌入 WebUI 运行时集成（2026-09-07 落地）；报告 → `tests/reports/client-desktop/` |
| E2E | `tests/e2e/`（下表） | `wptsall test desktop-e2e`（`--smoke\|--journey\|--all`） | 真 Tauri 窗口 / 真旅程；环境 fail-closed **exit 2** |

## E2E 用例清单（11 项）

| 脚本 | 用途 | 归属 |
|------|------|------|
| `tauri-smoke.sh` | 真 Tauri 窗口 DOM 冒烟：默认档全 8 页导航 sweep + **800×560 窄窗口汉堡导航回归档**（2026-09-25 §42 增） | `desktop-e2e --smoke`（默认档） |
| `docker-wp-journey.sh` | Lab WP → Protocol v2 → mock → 写回真旅程（有意区别于 WebUI 捷径） | `desktop-e2e --journey` |
| `run-auto-translate-parity.sh` | 共享 WebUI auto-translate API 路径 parity（无窗口） | support lane |
| `run-lab-cases-subset.sh` | 与 WebUI 同 `wizard-*` testid 的 lab-cases 子集 | support lane |
| `run-lifecycle-gate.sh` | 进程级生命周期门禁（G-16：对真二进制断言） | support lane |
| `run-pre-wp-subset.sh` | pre-WP 子集：Sites save+Test + Overview Run Once（共享 testid） | support lane |
| `tauri-commercial-forms-smoke.sh` | U1–U7 商业表单真窗口 DOM 填交 | support lane |
| `tauri-provider-wizard-from-lab-cases.sh` | mock lab-provider-cases（非硬编码）驱动 provider 向导 | support lane |
| `capture-desktop-visual.sh` | 视觉取证截图工具（诊断用，非断言门禁） | 工具 |
| `reports/` | 车道运行产物 | — |

## 环境前置（缺任一 → fail-closed exit 2，不假绿）

- **debug Desktop 二进制**（先构建）
- **`tauri-driver`** + **WebKitWebDriver**（或 `WPTSALL_TAURI_NATIVE_DRIVER` 指定）
- **Xvfb**（`xvfb-run` 真窗口链）
- 窗口类脚本需 **agent :8977**（官方常驻代理，device-id `e2e-lab-webui` 识别豁免见 `tests/README.md` §6.1）
- `--journey` 需 **Docker Lab**（`wptsall lab up`）+ **mock :9090**

## 上层口径

- lane 定位：`tests/README.md` §3（desktop-e2e 行）· §4（J 桶：Desktop 自有，与 E 桶（WebUI）同 cases 不同运行时，**必须独立**）
- 脚本纪律：一律经 `tests/lib/repo-root.sh` 解析仓库根
