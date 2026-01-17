ALTER TABLE games DISABLE ROW LEVEL SECURITY;
ALTER TABLE users DISABLE ROW LEVEL SECURITY;

DROP POLICY "Users can view games" on games;
DROP POLICY "Anyone can view profiles" on users;
DROP POLICY "Users can create games" on games;
DROP POLICY "Anyone can create profiles" on users;
DROP POLICY "Users can update their own games." on games;
DROP POLICY "Users can update their own profiles." on users;
DROP POLICY "Users can delete their games." on games;
DROP POLICY "Users can delete their profiles." on users;
