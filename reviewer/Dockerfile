# syntax=docker/dockerfile:1
# lightleaf-reviewer: the optional AI review service. Build from the repository root:
#
#   docker build -f reviewer/Dockerfile -t lightleaf-reviewer .
#
# It needs only git and outbound HTTPS (to the Claude API); no TeX, no data volume.

FROM rust:1-bookworm AS build
WORKDIR /src
# Dependencies first, so they are cached across source changes.
COPY Cargo.toml Cargo.lock ./
COPY reviewer/Cargo.toml reviewer/Cargo.toml
RUN mkdir -p src reviewer/src && echo 'fn main() {}' > src/main.rs && echo 'fn main() {}' > reviewer/src/main.rs \
 && cargo build --release -p lightleaf-reviewer \
 && rm -rf reviewer/src target/release/deps/lightleaf_reviewer* target/release/lightleaf-reviewer*
COPY reviewer ./reviewer
RUN cargo build --release -p lightleaf-reviewer && cp target/release/lightleaf-reviewer /lightleaf-reviewer

FROM debian:bookworm-slim
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* \
 && useradd --system --uid 10002 --create-home --home-dir /home/reviewer reviewer
COPY --from=build /lightleaf-reviewer /usr/local/bin/lightleaf-reviewer
USER reviewer
ENV LIGHTLEAF_REVIEWER_BIND=0.0.0.0:8090 \
    LIGHTLEAF_REVIEW_WORKDIR=/tmp/lightleaf-reviewer \
    RUST_LOG=lightleaf_reviewer=info
EXPOSE 8090
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8090 && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ok <&3"]
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["lightleaf-reviewer"]
