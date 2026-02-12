# Agent Guide for Retro Game Exchange

This guide describes how to work effectively in the `retro-game-exchange` repository.

## Project Overview

This is a Rust-based web application for exchanging retro games. It uses a Modular Monolith architecture where the same binary can run as different services (`Main`, `Auth`, `Email`) based on configuration.

- **Backend**: Rust (Axum, Tokio, Diesel Async)
- **Frontend**: HTML/JS (Parcel, Sailfish Templates, HTMX, PicoCSS)
- **Database**: PostgreSQL (with `pgvector` and `diesel_full_text_search`)
- **Infrastructure**: Docker Compose
- **Observability**: OpenTelemetry, Tracing (Tempo, Loki, Prometheus)

## Essential Commands

The project uses `just` as a command runner. Always prefer these over raw commands.

- **Start Infrastructure**: `just up` (starts Postgres, Kafka, Observability stack in Docker)
- **Run Backend**: `just run` (builds frontend & runs backend in debug mode)
- **Build All**: `just build` (builds frontend & backend)
- **Clean**: `just clean`
- **Frontend Only**: `just frontend` (installs deps & builds frontend)
- **Reset Telemetry**: `just clear-telemetry` (cleans up docker volumes)

## Codebase Structure

- `src/`: Rust backend source
    - `api/`: API route handlers (games, users, offers, auth)
    - `schema.rs`: Diesel schema (auto-generated)
    - `main.rs`: Application entry point, wiring, and service selection
    - `telemetry.rs`: OpenTelemetry setup
    - `kafka.rs`: Kafka producer/consumer logic
- `frontend/`: Frontend source & build
    - `src/`: Source files (`.html`, `.js`, `.stpl`)
    - `dist/`: Build output (served by backend)
    - `partials/`: HTMX partial templates
- `migrations/`: Diesel database migrations
- `docker/`: Dockerfiles for app and services
- `volumes/`: Persisted data for local Docker stack

## Development Patterns

### Backend (Rust)
- **Framework**: `axum` 0.8.x
- **Database**: `diesel-async` with connection pooling (`bb8`).
    - Use `diesel migration run` (via CLI) or `just up` (auto-runs migrations).
- **Service Types**: Controlled by `SERVICE_TYPE` env var or config.
    - `Main`: Core logic (Games, Offers).
    - `Auth`: Handles Login/Signup & JWT issuance.
    - `Email`: Consumes Kafka events to send emails.
- **Authentication**: JWT (ES256).
    - `Auth` service acts as the issuer.
    - `Main` and `Email` services fetch JWKs from `Auth` service to validate tokens.
- **Search & Embeddings**:
    - Uses an external embedding model service (configured via `EMBEDDING_MODEL_URL`).
    - Vectors are truncated to 256 dimensions and stored in Postgres via `pgvector`.
    - `just run -- --reembed` triggers a re-embedding of all games.
- **HTMX Support**: Custom layers (`ServeHtmxDir`, `HxRequestLayer`) handle HTMX-specific logic (partials, headers).

### Frontend
- **Build System**: Parcel (via `pnpm`).
- **Templating**: `sailfish` (`.stpl` files). Compiled to Rust code, but source lives in `frontend/`.
- **Styles**: `PicoCSS` (classless/minimal CSS).
- **Interactivity**: `htmx` for dynamic behavior without heavy JS bundles.

### Testing & Observability
- **Logs**: Structured JSON logs via `tracing`.
- **Tracing**: OTLP export to Tempo (visualize in Grafana).
- **Metrics**: Prometheus metrics exposed.

## Configuration
- `config.tson`: Main configuration file (TysonScript Object Notation).
- `diesel.toml`: Diesel CLI config.
- Environment variables override config (see `Cli` struct in `src/main.rs`).

## Gotchas
- **Frontend Build Required**: The backend expects `frontend/dist` to exist. `just run` handles this, but raw `cargo run` might fail if frontend isn't built.
- **Service Mode**: The app behaves differently depending on `SERVICE_TYPE`. Default is `Main`.
- **Template Compilation**: `sailfish` templates are compiled into the binary. Changes to `.stpl` files require a Rust recompile to take effect.
- **Database Extensions**: The database uses `pgvector` and `tsvector`. Ensure the Docker image has these extensions (handled in `docker/postgres`).

## Database Migrations
- Migrations are in `migrations/`.
- To create a new migration: `diesel migration generate <name>`
- To run migrations: `diesel migration run`
