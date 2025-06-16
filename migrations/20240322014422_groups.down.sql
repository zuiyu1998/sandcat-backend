-- Drop tables in reverse order to respect foreign key constraints
DROP TABLE IF EXISTS group_announcements;
DROP TABLE IF EXISTS group_invitations;
DROP TABLE IF EXISTS group_mute_records;
DROP TABLE IF EXISTS group_polls;
DROP TABLE IF EXISTS group_files;
DROP TABLE IF EXISTS group_members;
DROP TABLE IF EXISTS group_categories;
DROP TABLE IF EXISTS groups;

-- Drop custom types
DROP TYPE IF EXISTS group_role;
