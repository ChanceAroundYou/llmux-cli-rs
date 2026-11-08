# New API balance contract

This integration is intentionally explicit: select `newapi` (aliases `new-api` /
`new_api`) in the account balance-provider field. New API deployments may use
different routes or quota units; do not add host sniffing or silently probe
alternate URLs.

The field names below were checked against the current public
`QuantumNous/new-api` source (`controller/user.go`, `controller/token.go`,
`middleware/auth.go`, `middleware/access_token_routes.go`,
`router/api-router.go`). That proves the upstream shape at the time of writing,
**not** the version or customization of any particular deployment.

## Credential modes

The route is chosen by **where the credential came from**, never by what it
looks like (`sk-` prefix means nothing here):

- `balance_auth` (encrypted) is treated as a **user access token** and queries
  `/api/user/self` (wallet). It must be a dashboard access token with the
  `profile:read` scope — without that scope New API answers 403.
- When `balance_auth` is empty, the encrypted account **API key** is used to
  query `/api/usage/token/` (token quota only, `TokenAuthReadOnly`).
- Token quota is never displayed as currency or described as wallet balance.

## Current contract

Account endpoints point at the OpenAI-compatible surface. The deployment base
is derived by stripping the API surface and keeping any deployment prefix:

| account endpoint | management base |
|---|---|
| `https://host` | `https://host` |
| `https://host/v1` | `https://host` |
| `https://host/v1/chat/completions` | `https://host` |
| `https://host/newapi/v1` | `https://host/newapi` |

Unparseable or non-`http(s)` endpoints fail the probe; there is no hard-coded
fallback host. Requests send:

```text
Authorization: Bearer <credential>
Accept: application/json
```

- Wallet (`balance_auth`): `GET {base}/api/user/self`, envelope
  `{"success": true, "data": {...}}` with `data.quota` and `data.used_quota`.
- Token quota (API key): `GET {base}/api/usage/token/` (trailing slash
  included), envelope `{"code": true, "message": "...", "data": {...}}` with
  `data.total_available`, `data.total_used`, and `data.unlimited_quota`.
  `data.user_quota` may appear only when the token allows wallet queries; this
  adapter deliberately ignores it and shows token quota only.

Field semantics:

- `quota` is the **remaining** user wallet quota. `used_quota` is cumulative
  usage; it is displayed separately and is **not** subtracted from `quota`
  again.
- The current public `GET /api/user/self` response has no per-user
  `quota_per_unit`. `QuotaPerUnit` is a global system setting, not a field of
  this response, so the adapter shows raw quota and never assumes a currency
  conversion.
- Quota values may be JSON numbers or numeric strings. Missing, negative,
  non-finite, malformed, unauthorized, non-2xx, or unsuccessful responses fail
  closed — no default `0`, no empty success card, and the credential is never
  echoed in an error message.

## Updating the contract

If a deployment exposes a different versioned endpoint or field names, update
the constants and pure parser fixtures in `crates/llmux-core/src/balance.rs`
and `crates/llmux-core/tests/balance_contract.rs` from a **脱敏** protocol
sample. Do not add production URLs, credentials, passwords, or real response
bodies to this repository.
