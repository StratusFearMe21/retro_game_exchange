ALTER TABLE offers DISABLE ROW LEVEL SECURITY;

DROP POLICY "Users can view offers" on offers;
DROP POLICY "Users can create offers" on offers;
DROP POLICY "Users can update their own offers." on offers;
DROP POLICY "Users can delete their offers." on offers;
DROP POLICY "Users can offer only their own games" on offers;
DROP POLICY "Users offers cannot be updated to be somebody else's game." on offers;


DROP TABLE offers;
DROP TYPE offer_status;
