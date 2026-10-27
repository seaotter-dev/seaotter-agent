# syntax=docker/dockerfile:1

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p ticket-router -p gated-generate -p seaotter-agent -p effects \
    && mkdir -p /out \
    && cp target/release/ticket-router target/release/gated-generate target/release/seaotter-agent target/release/effects /out/

FROM alpine:3
RUN apk add --no-cache ca-certificates
COPY --from=build /out/ticket-router /out/gated-generate /out/seaotter-agent /out/effects /usr/local/bin/
COPY examples/ticket-router/fixtures /src/examples/ticket-router/fixtures
COPY examples/gated-generate/fixtures /src/examples/gated-generate/fixtures
RUN ln -s seaotter-agent /usr/local/bin/seaotter \
    && addgroup -g 10001 -S seaotter \
    && adduser -u 10001 -S -D -H -G seaotter seaotter \
    && mkdir -p /data /workspace \
    && chown 10001:10001 /data /workspace
USER seaotter
WORKDIR /workspace
VOLUME /workspace
ENTRYPOINT ["/usr/local/bin/seaotter"]
HEALTHCHECK --interval=5s --timeout=3s --retries=3 \
    CMD ["/usr/local/bin/seaotter", "--health"]
