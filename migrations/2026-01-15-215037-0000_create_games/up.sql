CREATE TYPE condition AS ENUM ('mint', 'good', 'fair', 'poor');

CREATE TABLE games(
    id SERIAL PRIMARY KEY,
    name VARCHAR NOT NULL,
    publisher VARCHAR,
    year SMALLINT,
    platform VARCHAR,
    condition condition
);
