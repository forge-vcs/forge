ALTER TABLE repositories ADD COLUMN content_backend TEXT NOT NULL DEFAULT 'git';
ALTER TABLE current_state ADD COLUMN attached_attempt_id TEXT REFERENCES attempts(id);
