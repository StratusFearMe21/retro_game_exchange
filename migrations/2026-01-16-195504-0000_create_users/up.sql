CREATE TABLE users(
    id SERIAL PRIMARY KEY,
    username VARCHAR NOT NULL UNIQUE,
    street_address VARCHAR,
    password BYTEA NOT NULL
)
