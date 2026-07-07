CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL
);

CREATE VIEW active_users AS
SELECT id, name, email FROM users WHERE name IS NOT NULL;

CREATE PROCEDURE get_user(IN user_id INTEGER)
BEGIN
    SELECT * FROM users WHERE id = user_id;
END;

CREATE PROCEDURE report_active_users()
BEGIN
    CALL get_user(1);
END;
