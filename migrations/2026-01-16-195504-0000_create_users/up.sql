CREATE TABLE users(
    id SERIAL PRIMARY KEY,
    username VARCHAR NOT NULL UNIQUE,
    mailing_address_1 VARCHAR,
    mailing_address_2 VARCHAR,
    city VARCHAR,
    state CHAR(2),
    zip VARCHAR,
    pbkdf2_iterations INTEGER NOT NULL,
    salt BYTEA NOT NULL,
    password BYTEA NOT NULL
)
