-- Workspaces: a folder a chat focuses on (in the person's RAST files, or on
-- the server for administrators). Its chats' file tools stay inside it, and
-- commands start in it.
CREATE TABLE ai_workspaces (
    id         TEXT    PRIMARY KEY,
    user_id    INTEGER NOT NULL,
    name       TEXT    NOT NULL,
    place      TEXT    NOT NULL CHECK (place IN ('files', 'server')),
    path       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX ai_workspaces_user ON ai_workspaces(user_id, name);

ALTER TABLE ai_conversations ADD COLUMN workspace_id TEXT;
CREATE INDEX ai_conversations_workspace ON ai_conversations(workspace_id);
