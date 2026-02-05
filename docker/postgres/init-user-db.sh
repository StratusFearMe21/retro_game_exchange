#!/usr/bin/env bash
set -e

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
	CREATE USER docker WITH PASSWORD '$POSTGRES_PASSWORD';
	CREATE USER otelu WITH PASSWORD '$POSTGRES_PASSWORD';
	CREATE DATABASE docker;
	GRANT ALL PRIVILEGES ON DATABASE docker TO docker;
	GRANT pg_monitor TO otelu;
EOSQL

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "docker" <<-EOSQL
	CREATE EXTENSION vector;
	CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
	GRANT USAGE, CREATE ON SCHEMA public TO docker;
EOSQL

