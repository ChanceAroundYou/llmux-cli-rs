# LLMux Rust Foundation Verification

Run these checks before starting the provider-adapter migration plan.

## Automated

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

Expected: all commands pass.

## Manual

Start the server:

```bash
cargo run -p llmux -- start --port 25975
```

Check health:

```bash
curl http://localhost:25975/api/health
```

Expected:

```json
[]
```

Import old-compatible config:

```bash
curl -X POST http://localhost:25975/api/import \
  -H "content-type: application/json" \
  -d '{"version":1,"accounts":[{"alias":"main","provider_id":"openai","api_key":"sk-test"}],"aliases":[],"keys":[],"settings":[]}'
```

Expected response includes:

```json
{"success":true,"imported":{"accounts":1,"aliases":0,"keys":0}}
```

Export config:

```bash
curl http://localhost:25975/api/export
```

Expected response includes a plaintext account key in the old export shape:

```json
{"version":1,"accounts":[{"alias":"main","provider_id":"openai","api_key":"sk-test"}]}
```

Open UI:

```
http://localhost:25975/
```

Expected: HTML loads (placeholder or built UI).

## Contract summary

- Config: `PORT=25975`, `DATA_DIR` platform-specific, `MASTER_KEY` optional
- Crypto: AES-256-GCM + scrypt, `get_or_create_master_key` persists to `master.key`
- DB: SQLite with 7 tables, `init_db` creates schema + seeds 4 providers
- Export/Import: JSON `{version, accounts, aliases, keys, settings}` shape preserved
- Server: 27 API routes + SPA fallback, `/v1` routes return 401 without auth
