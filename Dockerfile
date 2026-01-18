FROM node:20-alpine AS node

# Install dependencies only when needed
FROM node AS deps
# Check https://github.com/nodejs/docker-node/tree/b4117f9333da4138b03a546ec926ef50a31506c3
#nodealpine to understand why libc6-compat might be needed.
RUN apk add --no-cache libc6-compat
WORKDIR /app

# Install dependencies from pnpm
COPY frontend/package.json frontend/pnpm-lock.yaml* frontend/pnpm-workspace.yaml* frontend/.npmrc* ./frontend/
COPY frontend/posthtml-lucide ./frontend/posthtml-lucide/
COPY frontend/parcel-packager-sailfish ./frontend/parcel-packager-sailfish/
RUN corepack enable pnpm && pnpm i --frozen-lockfile -C frontend

# Rebuild the source code only when needed
FROM node AS builder-js
WORKDIR /app
COPY --from=deps /app/frontend/node_modules ./frontend/node_modules
COPY --from=deps /app/frontend/posthtml-lucide/node_modules ./frontend/posthtml-lucide/node_modules
COPY --from=deps /app/frontend/parcel-packager-sailfish/node_modules ./frontend/parcel-packager-sailfish/node_modules
COPY --from=deps /app/frontend/posthtml-lucide/dist ./frontend/posthtml-lucide/dist
COPY ./frontend ./frontend

# Next.js collects completely anonymous telemetry data about general usage.
# Learn more here: https://nextjs.org/telemetry
# Uncomment the following line in case you want to disable telemetry during the build.
# ENV NEXT_TELEMETRY_DISABLED=1

RUN corepack enable pnpm && pnpm run -C frontend build

FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder-rs 
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
COPY --from=builder-js /app/frontend/dist frontend/dist
RUN cargo build --release --bin retro-game-exchange

# We do not need the Rust toolchain to run the binary!
FROM debian:trixie-slim AS runtime
WORKDIR /app
COPY --from=builder-rs /app/target/release/retro-game-exchange /usr/local/bin
COPY --from=builder-js /app/frontend/dist frontend/dist

EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/retro-game-exchange"]
