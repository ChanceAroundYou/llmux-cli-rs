FROM debian:trixie-slim

# Local-modified llmux binary with Anthropic→OpenAI conversion
COPY llmux /usr/local/bin/llmux
RUN chmod +x /usr/local/bin/llmux

ENV DATA_DIR=/data/llmux
RUN mkdir -p /data/llmux

EXPOSE 25976

CMD ["/usr/local/bin/llmux", "start", "--no-tui"]
