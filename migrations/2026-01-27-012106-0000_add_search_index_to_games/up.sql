CREATE OR REPLACE FUNCTION immutable_tsvector(
  name VARCHAR, 
  publisher VARCHAR, 
  year smallint, 
  platform VARCHAR, 
  condition condition
)
RETURNS tsvector AS $$
  -- We hardcode 'english' here so the function signature 
  -- only requires the input text.
  SELECT to_tsvector('english',
    concat_ws(' ', name, publisher, year::text,
    platform, condition::text)
  );
$$ LANGUAGE sql IMMUTABLE;

ALTER TABLE games 
ADD COLUMN tsembedding tsvector NOT NULL
GENERATED ALWAYS AS (
  immutable_tsvector(name, publisher, year, platform, condition)
) STORED;


-- https://supabase.com/docs/guides/ai/hybrid-search
create or replace function hybrid_search(
  query_text text,
  query_embedding vector(256),
  match_count int,
  full_text_weight float = 1,
  semantic_weight float = 1,
  rrf_k int = 50
)
returns setof games
language sql
as $$
with full_text as (
  select
    id,
    -- Note: ts_rank_cd is not indexable but will only rank matches of the where clause
    -- which shouldn't be too big
    row_number() over(order by ts_rank_cd(tsembedding, websearch_to_tsquery(query_text)) desc) as rank_ix
  from
    games
  where
    tsembedding @@ websearch_to_tsquery(query_text)
  order by rank_ix
  -- limit least(match_count, 30) * 2
),
semantic as (
  select
    id,
    row_number() over (order by embedding <#> query_embedding) as rank_ix
  from
    games
  order by rank_ix
  -- limit least(match_count, 30) * 2
)
select
  games.*
from
  full_text
  full outer join semantic
    on full_text.id = semantic.id
  join games
    on coalesce(full_text.id, semantic.id) = games.id
order by
  coalesce(1.0 / (rrf_k + full_text.rank_ix), 0.0) * full_text_weight +
  coalesce(1.0 / (rrf_k + semantic.rank_ix), 0.0) * semantic_weight
  desc
-- limit
--   least(match_count, 30)
$$;

CREATE INDEX games_idx_fts ON games USING GIN (tsembedding);
CREATE INDEX games_idx_embeddings ON games
    USING hnsw(embedding vector_ip_ops);
