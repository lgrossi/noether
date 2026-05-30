FROM rust:1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --locked --release --bin noet

FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --home /var/lib/noet --create-home --shell /usr/sbin/nologin noet \
  && mkdir -p /etc/noet /var/lib/noet \
  && chown -R noet:noet /var/lib/noet

COPY --from=builder /src/target/release/noet /usr/local/bin/noet
COPY examples/deployment/noet-container-config.yaml /etc/noet/config.yaml
COPY examples/deployment/noet-container-policy.yaml /etc/noet/policy.yaml

EXPOSE 4051
USER noet
ENTRYPOINT ["noet"]
CMD ["up", "--config", "/etc/noet/config.yaml"]
