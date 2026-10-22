ALTER TABLE profiles ADD COLUMN role TEXT NOT NULL DEFAULT 'member'
    CHECK (role IN ('owner', 'member', 'guest', 'developer'));

-- The primary member owns the pond; without one, the oldest profile does, so no household is left ownerless.
UPDATE profiles SET role = 'owner'
WHERE id = (SELECT value FROM settings WHERE key = 'primary_profile_id');

UPDATE profiles SET role = 'owner'
WHERE NOT EXISTS (SELECT 1 FROM profiles WHERE role = 'owner')
  AND id = (SELECT id FROM profiles ORDER BY created_at ASC, rowid ASC LIMIT 1);
