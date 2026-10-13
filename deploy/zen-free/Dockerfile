FROM node:22-slim

# zen-free sidecar: Node TLS client for the OpenCode Zen free tier.
# The Rust gateway cannot talk to this upstream directly (TLS fingerprint
# gate), so it lives as a compose-internal service, like codex-api.
WORKDIR /app

COPY package.json ./
RUN npm install --omit=dev

COPY server.mjs ./

# Loopback-only inside the container: the service is reachable only through
# the compose network (llmux -> zen-free:6446). Never publish this port.

EXPOSE 6446

CMD ["node", "server.mjs"]
