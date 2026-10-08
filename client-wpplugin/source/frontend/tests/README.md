# Client Frontend Test Layout

Status: archival marker only.

- `archived-cross-system/` - historical cross-system E2E source suites retained for reference only
- active client page-flow Playwright suites have moved to `dev-tool/e2e/playwright/`
  - `support-client-local-ui/`
  - `support-client-server/`
  - `support-client-mock/`

Current migration rule:

- new page-flow coverage belongs under `dev-tool/e2e/`
- this directory no longer hosts active page-flow suites
- see `AGENTS.md` and `task/2026-03-11/TEST-DIRECTORY-MIGRATION.md`

Canonical commands:

```bash
cd client/frontend && npm run test:unit
cd client/frontend && npm run test:playwright:local
cd client/frontend && npm run test:playwright:server
cd client/frontend && npm run test:playwright:mock
```

Official cross-system gate does not run from this directory. Use:

```bash
bash dev-tool/e2e/run.sh
```
