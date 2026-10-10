FROM node:22-bookworm-slim AS web
WORKDIR /build/frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS server
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY src/ src/
COPY migrations/ migrations/
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 --home-dir /var/lib/postwarden postwarden && mkdir -p /var/lib/postwarden && chown postwarden:postwarden /var/lib/postwarden
COPY --from=server /build/target/release/postwarden /usr/local/bin/postwarden
COPY --from=web /build/frontend/dist /opt/postwarden/frontend/dist
USER postwarden
WORKDIR /var/lib/postwarden
ENV POSTWARDEN_LISTEN=0.0.0.0:8080 POSTWARDEN_WEB_DIST=/opt/postwarden/frontend/dist DATABASE_URL=sqlite://data/postwarden.db POSTWARDEN_COOKIE_SECURE=true
EXPOSE 8080
CMD ["postwarden"]
