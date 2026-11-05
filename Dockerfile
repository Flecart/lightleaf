# syntax=docker/dockerfile:1
# Lightleaf — multi-stage build producing a small runtime image with TeX Live, git and Typst.
#
#   docker build -t lightleaf .                                   # full TeX selection (~1.5 GB)
#   docker build --build-arg TEX_PACKAGES="texlive-latex-recommended latexmk" -t lightleaf:slim .

# ---- frontend ----------------------------------------------------------------------------
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

# ---- backend -----------------------------------------------------------------------------
FROM rust:1-bookworm AS server
WORKDIR /src
# Build dependencies first so they are cached across source changes.
COPY Cargo.toml Cargo.lock ./
# The workspace also lists the (separately built) reviewer crate; a stub is enough here.
COPY reviewer/Cargo.toml reviewer/Cargo.toml
RUN mkdir -p src frontend/dist reviewer/src && echo 'fn main() {}' > src/main.rs \
 && echo 'fn main() {}' > reviewer/src/main.rs \
 && cargo build --release && rm -rf src target/release/deps/lightleaf* target/release/lightleaf*
COPY src ./src
COPY --from=web /web/dist ./frontend/dist
RUN cargo build --release && cp target/release/lightleaf /lightleaf

# ---- runtime -----------------------------------------------------------------------------
FROM debian:bookworm-slim
ARG TARGETARCH
ARG TYPST_VERSION=0.13.1
ARG TEX_PACKAGES="texlive-latex-recommended texlive-latex-extra texlive-fonts-recommended texlive-science texlive-xetex texlive-luatex texlive-bibtex-extra texlive-lang-european biber latexmk"
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates git tini curl xz-utils perl fontconfig ${TEX_PACKAGES} \
 && arch=$([ "$TARGETARCH" = "arm64" ] && echo aarch64 || echo x86_64) \
 && curl -fsSL "https://github.com/typst/typst/releases/download/v${TYPST_VERSION}/typst-${arch}-unknown-linux-musl.tar.xz" \
      | tar -xJ --strip-components=1 -C /usr/local/bin "typst-${arch}-unknown-linux-musl/typst" \
 && apt-get purge -y curl xz-utils && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* \
 && useradd --system --uid 10001 --create-home --home-dir /home/lightleaf lightleaf \
 && mkdir -p /data && chown lightleaf:lightleaf /data
COPY --from=server /lightleaf /usr/local/bin/lightleaf
USER lightleaf
# Warm the LuaTeX font cache so the first LuaLaTeX/fontspec compile is fast.
RUN (command -v luaotfload-tool >/dev/null && luaotfload-tool --update >/dev/null 2>&1) || true
ENV LIGHTLEAF_DATA=/data \
    LIGHTLEAF_BIND=0.0.0.0:8080 \
    RUST_LOG=lightleaf=info
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /api/health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ok <&3"]
# tini reaps orphaned compiler processes (latexmk/biber children) and forwards signals.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["lightleaf"]
