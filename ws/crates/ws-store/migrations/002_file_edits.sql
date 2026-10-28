-- File edits the agent made through approved tools, kept so each can be shown
-- as a diff and undone. App bookkeeping, not conversation content: the model
-- never sees this table.
CREATE TABLE file_edits (
    -- The tool call that made the edit.
    call_id         TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    -- Absolute path of the file that changed.
    path            TEXT NOT NULL,
    -- Blob hashes of the exact bytes. NULL before: the tool created the file.
    -- NULL after: the tool deleted it.
    before_blob     TEXT,
    after_blob      TEXT,
    undone          INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL
);

CREATE INDEX file_edits_by_conversation ON file_edits(conversation_id);
