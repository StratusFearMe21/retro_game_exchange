ALTER TABLE games ENABLE ROW LEVEL SECURITY;
ALTER TABLE games FORCE ROW LEVEL SECURITY;
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE users FORCE ROW LEVEL SECURITY;

CREATE POLICY "Users can view games"
ON games FOR SELECT
USING ( (SELECT current_setting('app.current_user_id', true)::integer) != 0);

CREATE POLICY "Anyone can view profiles"
ON users FOR SELECT
USING ( true );

CREATE POLICY "Users can create games"
ON games FOR INSERT
WITH CHECK ( (SELECT current_setting('app.current_user_id', true)::integer) = owned_by);

CREATE POLICY "Anyone can create profiles"
ON users FOR INSERT
WITH CHECK ( true );

CREATE POLICY "Users can update their own games."
ON games FOR UPDATE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = owned_by)
WITH CHECK ( (SELECT current_setting('app.current_user_id', true)::integer) = owned_by);

CREATE POLICY "Users can update their own profiles."
ON users FOR UPDATE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = id)
WITH CHECK ( (SELECT current_setting('app.current_user_id', true)::integer) = id);

CREATE POLICY "Users can delete their games."
ON games FOR DELETE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = owned_by);

CREATE POLICY "Users can delete their profiles."
ON users FOR DELETE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = id);
