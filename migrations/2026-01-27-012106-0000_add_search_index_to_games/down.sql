DROP INDEX games_idx_fts;
DROP INDEX games_idx_embeddings;

ALTER TABLE games 
DROP COLUMN tsembedding;
