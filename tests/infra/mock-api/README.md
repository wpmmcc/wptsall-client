# mock-translate-api

This is a **standalone test-only** mock translation API. It simulates multiple
translation vendor APIs (Baidu, Youdao, Tencent, AWS, Azure, Kakao, etc.) with
real authentication algorithm verification.

## When to use this

Use this standalone service **only** for isolated testing scenarios, such as:

- CI pipelines that verify per-vendor auth algorithms without spinning up the
  full WPTSALL Server.
- Integration tests that need a lightweight translation mock on a dedicated port
  (`:9090`).

## For development

This service is the unified local mock target for:

- official mock component templates
- official vendor templates rewritten to local `127.0.0.1:9090`
- signing algorithm verification
- client E2E and local smoke flows

The goal is to keep official component request structure unchanged except for
the request host/path rewrite to local mock.

## Running

```bash
cd /home/john/wpmmcc-ats3.0/tests/infra/mock-api
MOCK_TRANSLATE_HOST=127.0.0.1 cargo run
# Defaults to :9090; use MOCK_TRANSLATE_PORT for an owned test instance.
```

`MOCK_TRANSLATE_HOST` selects the bind host (legacy default `0.0.0.0`).
Isolated local tests should explicitly bind `127.0.0.1`, use a separate port
and `MOCK_MEDIA_DIR`, and leave existing shared mock processes running.

## Mock behavior

- Text: wraps translated text with language markers: `【zh】...【/zh】`.
- HTML text: only wraps text nodes (content inside tags), keeps tags unchanged.
- Media refs: returns the same `source_ref` with `-{target_lang}` appended before extension.
  - Example: `https://cdn/a/image.png` -> `https://cdn/a/image-zh.png`
- The same media response also returns `translated_text`: OCR, subtitle, transcript, or document text. That string is not the file URL.
- **Multi-auth vendors** are split into mock **profiles** (Google key vs OAuth, Azure subscription vs OAuth).
  - Inventory: `GET /api/v1/mock-auth-profiles`
  - Profile paths: `/mock/profiles/{id}/…`
  - Docs: `docs/_archive/tasks/test/11-MOCK-AUTH-PROFILES.md`（历史；现行以本 README + mock 代码为准）
- Input length: profiles enforce `max_input_chars` (400 `input_too_long`).

## Tests

```bash
cargo test --lib
cargo test --tests
# or from repo root:
bash scripts/mock-catalog-parity-e2e.sh
```

## Bearer auth mode

- Default mode: accepts any non-empty `Authorization: Bearer ...` token.
- Strict mode (recommended for auth testing):
  - set `MOCK_TRANSLATE_STRICT_BEARER=1`
  - set `MOCK_TRANSLATE_API_KEY=<your-key>`
  - then Bearer token must exactly match `MOCK_TRANSLATE_API_KEY`
