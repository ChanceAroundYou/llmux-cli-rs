# New API balance contract

This integration is intentionally explicit: select `newapi` (aliases `new-api` /
`new_api`) in the account balance-provider field. New API deployments may use
different routes or quota units; do not add host sniffing or silently probe
alternate URLs.

The field names below were checked against the current public
`QuantumNous/new-api` source (`controller/user.go`, `controller/token.go`,
`controller/misc.go`, `middleware/auth.go`, `middleware/access_token_routes.go`,
`router/api-router.go`). That proves the upstream shape at the time of writing,
**not** the version or customization of any particular deployment.

## Credential modes

The route is chosen by **where the credential came from**, never by what it
looks like (`sk-` prefix means nothing here):

- `balance_auth` (encrypted) is treated as a **user access token** and queries
  `/api/user/self` (wallet). It must be a dashboard access token with the
  `profile:read` scope — without that scope New API answers 403.
- When `balance_auth` is empty, the encrypted account **API key** is used to
  query `/api/usage/token/` (`TokenAuthReadOnly`).

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
  `data.quota` is the **remaining** user wallet quota; `used_quota` is
  cumulative usage and is shown separately, never subtracted from `quota` a
  second time.
- Token usage (API key): `GET {base}/api/usage/token/` (trailing slash
  included), envelope `{"code": true, "message": "...", "data": {...}}` with
  `data.total_available`, `data.total_used`, and `data.unlimited_quota`.

### Wallet quota through the token endpoint

Upstream's `GetTokenUsage` nests `data.user_quota.total_available` /
`total_used` (the token owner's wallet) **whenever the token has
`allow_user_quota_query`**. That flag defaults to `true` for newly created
tokens and can be turned off per token. The adapter therefore:

1. shows `user_quota` as the wallet balance whenever it is present — including
   (in fact especially) when the key itself is `unlimited_quota`, because
   "unlimited" then only describes the per-key cap, not the wallet;
2. otherwise falls back to the token's own quota and labels it as such —
   token quota is never described as wallet balance;
3. fails closed if `user_quota` is present but incomplete; it never skips it
   silently in favour of the token figure.

### Money conversion

Neither `GET /api/user/self` nor `GET /api/usage/token/` carries a per-user
`quota_per_unit`. The public `GET /api/status` response does
(`data.quota_per_unit`, no credential needed), and upstream's own channel
balance logic converts with exactly that factor (this is also the figure its
front end displays). The adapter therefore also fetches `GET {base}/api/status`
— without sending the credential — and:

1. shows the wallet as USD (`$xx.xx`, with the raw quota and the factor in
   the detail line) when the factor is a finite positive number;
2. otherwise shows the raw quota as-is, unconverted. It never guesses a
   currency unit: no factor means no dollar figure, but the probe still
   succeeds with the raw quota.

Quota values may be JSON numbers or numeric strings. Missing, negative,
non-finite, malformed, unauthorized, non-2xx, or unsuccessful responses fail
closed — no default `0`, no empty success card, and the credential is never
echoed in an error message.

## Updating the contract

If a deployment exposes a different versioned endpoint or field names, update
the constants and pure parser fixtures in `crates/llmux-core/src/balance.rs`
and `crates/llmux-core/tests/balance_contract.rs` from a **脱敏** protocol
sample. Do not add production URLs, credentials, passwords, or real response
bodies to this repository.
