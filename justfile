build configuration="debug": frontend (backend configuration)

frontend:
    pnpm -C frontend install
    pnpm -C frontend run build

backend configuration="debug":
    cargo build {{ if configuration == "release" { "--release" } else { "" } }}

clean:
    cargo clean

run configuration="debug": frontend (backend configuration)
    cargo run {{ if configuration == "release" { "--release" } else { "" } }}

serve configuration="debug":
    bacon run-long -- {{ configuration }}
