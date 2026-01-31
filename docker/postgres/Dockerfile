FROM postgres:latest

RUN apt-get update \
      && apt-cache showpkg postgresql-$PG_MAJOR-pgvector \
      && apt-get install -y --no-install-recommends \
           postgresql-$PG_MAJOR-pgvector \
      && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /docker-entrypoint-initdb.d
COPY ./init-user-db.sh /docker-entrypoint-initdb.d/10_userdb.sh
