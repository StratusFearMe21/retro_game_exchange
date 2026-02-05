CREATE TYPE offer_status AS ENUM ('up', 'accepted', 'rejected');

CREATE TABLE offers(
    offer_up INTEGER NOT NULL,
    for_game INTEGER NOT NULL,
    made_by INTEGER NOT NULL,
    offer_status offer_status NOT NULL,
    PRIMARY KEY(offer_up, for_game),
    FOREIGN KEY (offer_up) REFERENCES games(id),
    FOREIGN KEY (for_game) REFERENCES games(id),
    FOREIGN KEY (made_by) REFERENCES users(id)
);

ALTER TABLE offers FORCE ROW LEVEL SECURITY;

CREATE POLICY "Users can view offers"
ON offers FOR SELECT
USING ( (SELECT current_setting('app.current_user_id', true)::integer) != 0);

CREATE POLICY "Users can create offers"
ON offers FOR INSERT
WITH CHECK ( (SELECT current_setting('app.current_user_id', true)::integer) = made_by);

CREATE POLICY "Users can offer only their own games"
ON offers FOR INSERT
WITH CHECK ( EXISTS (SELECT 1 FROM games WHERE owned_by = made_by AND id = offer_up) );

CREATE POLICY "Users can update their own offers."
ON offers FOR UPDATE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = made_by)
WITH CHECK ( (SELECT current_setting('app.current_user_id', true)::integer) = made_by);

CREATE POLICY "Users offers cannot be updated to be somebody else's game."
ON offers FOR UPDATE
WITH CHECK ( EXISTS (SELECT 1 FROM games WHERE owned_by = made_by AND id = offer_up) );

CREATE POLICY "Users can delete their offers."
ON offers FOR DELETE
USING ( (SELECT current_setting('app.current_user_id', true)::integer) = made_by);
