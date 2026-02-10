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

up:
    docker compose up --build -d

clear-telemetry:
    -docker volume rm retro-game-exchange_prom_data
    -docker volume rm retro-game-exchange_tempo_data
    -docker volume rm retro-game-exchange_automq_data
    docker compose up -d rustfs rc
    docker compose exec rc /usr/bin/rc rm -r --force rustfs/automq-data;
    docker compose exec rc /usr/bin/rc rm -r --force rustfs/automq-ops;
    docker compose exec rc /usr/bin/rc rm -r --force rustfs/tempo;
    docker compose exec rc /usr/bin/rc rm -r --force rustfs/loki-data;
    docker compose exec rc /usr/bin/rc rm -r --force rustfs/loki-ruler;
    docker compose down

