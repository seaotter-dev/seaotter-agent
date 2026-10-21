# syntax=docker/dockerfile:1

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p ticket-router -p gated-generate \
    && mkdir -p /out \
    && cp target/release/ticket-router target/release/gated-generate /out/

FROM alpine:3
RUN apk add --no-cache ca-certificates
COPY --from=build /out/ticket-router /out/gated-generate /usr/local/bin/
COPY examples/ticket-router/fixtures /src/examples/ticket-router/fixtures
COPY examples/gated-generate/fixtures /src/examples/gated-generate/fixtures
