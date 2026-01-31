CREATE TYPE condition AS ENUM ('mint', 'good', 'fair', 'poor');

DO $$
BEGIN
    -- Attempt operation
    CREATE EXTENSION IF NOT EXISTS vector;
EXCEPTION
    WHEN insufficient_privilege THEN
        RAISE NOTICE 'Permission denied, skipping extension creation.';
END $$;

CREATE TABLE games(
    id SERIAL PRIMARY KEY,
    name VARCHAR NOT NULL,
    publisher VARCHAR,
    year SMALLINT,
    platform VARCHAR,
    embedding vector(256) NOT NULL,
    condition condition
);
