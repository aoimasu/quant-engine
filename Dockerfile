# Deployment-agnostic image (QE-013): builds the workspace and runs the SAME `qe` binary as the
# local `cargo run -p qe-cli`. No platform-specific assumptions — state lives under configurable,
# mountable volume directories (paths come from config / `QE_`-prefixed env), so this runs
# identically on a laptop, Railway, or anywhere else.

# ---- builder ------------------------------------------------------------------------------------
FROM rust:1.96 AS builder
WORKDIR /build
# Copy the whole workspace and build the CLI in release mode.
COPY . .
RUN cargo build --release --locked -p qe-cli

# ---- runtime ------------------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
WORKDIR /app
# Code-commit provenance (QE-420): the build context may not ship `.git`, so `build.rs` can't resolve
# the SHA inside the image. Thread the real commit in at build time and expose it as an env override
# that `qe` reads at runtime (takes precedence over the compiled-in QE_BUILD_GIT_SHA):
#   docker build --build-arg QE_CODE_COMMIT="$(git rev-parse --short=12 HEAD)" .
# Unset ARG => empty env => `qe` falls back to the compiled SHA, then the crate version.
ARG QE_CODE_COMMIT=""
ENV QE_CODE_COMMIT=$QE_CODE_COMMIT
# The same binary the local run uses.
COPY --from=builder /build/target/release/qe /usr/local/bin/qe
# A default config; override by mounting your own and/or `QE_`-prefixed env vars.
COPY config.example.toml /app/config.toml
# Persistent state lives under /app/data — mount a volume here (paths are configurable in config.toml).
VOLUME ["/app/data"]

# `docker run <image> train --config config.toml` == the documented local run.
ENTRYPOINT ["qe"]
CMD ["train", "--config", "config.toml"]
