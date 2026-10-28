-- Initial schema (master spec s10).
--
-- Timestamps are RFC 3339 UTC text so the database stays readable by hand.
--
-- Ids are ULID strings. A ULID's timestamp has millisecond resolution and its
-- remaining 80 bits are random, so two ids minted in the same millisecond --
-- which is exactly what regenerating a reply does -- do NOT sort in creation
-- order. Anything needing insertion order must use rowid, not id.

CREATE TABLE conversations (
    id               TEXT PRIMARY KEY NOT NULL,
    title            TEXT NOT NULL,
    system_prompt    TEXT,
    -- Mutable pointer into the message tree; the visible thread is root -> this.
    active_leaf_id   TEXT,
    current_backend  TEXT NOT NULL,
    current_model_id TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
) STRICT;

CREATE TABLE messages (
    id                   TEXT PRIMARY KEY NOT NULL,
    conversation_id      TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    parent_id            TEXT REFERENCES messages(id),
    role                 TEXT NOT NULL CHECK (role IN ('system','user','assistant','tool')),
    author_backend       TEXT,
    author_model_id      TEXT,
    -- Canonical Vec<Part>, serialised.
    parts_json           TEXT NOT NULL,
    -- The provider's own reassembled response content, kept verbatim for
    -- same-model replay. Rebuilding this from parts_json risks 4xx errors on
    -- providers that sign their reasoning blocks (master spec s5.2).
    raw_provider_payload TEXT,
    status               TEXT NOT NULL CHECK (status IN ('complete','cancelled','failed')),
    status_reason        TEXT,
    created_at           TEXT NOT NULL
) STRICT;

-- Path reconstruction walks parent links; branch navigation lists siblings.
CREATE INDEX idx_messages_conversation ON messages(conversation_id, id);
CREATE INDEX idx_messages_parent ON messages(conversation_id, parent_id, id);

-- Invariant 1 (master spec s5.1): messages are immutable once persisted.
-- BEFORE UPDATE OF fires only when one of these columns appears in the SET
-- list, so status corrections and other bookkeeping stay possible while
-- content, lineage and role cannot change.
CREATE TRIGGER messages_content_immutable
BEFORE UPDATE OF parent_id, role, parts_json, raw_provider_payload ON messages
BEGIN
    SELECT RAISE(ABORT, 'messages are immutable: regenerate creates a sibling instead');
END;

-- Invariant 5: deleting a whole conversation is the only destructive
-- operation. A cascade from conversations removes the parent row first, so
-- the guard below sees no conversation and allows it; a direct delete of one
-- message from a live conversation is refused.
CREATE TRIGGER messages_no_single_delete
BEFORE DELETE ON messages
WHEN EXISTS (SELECT 1 FROM conversations WHERE id = OLD.conversation_id)
BEGIN
    SELECT RAISE(ABORT, 'messages cannot be deleted individually');
END;

-- An in-progress reply lives here, checkpointed about once a second, until it
-- completes. On launch, leftovers become Failed messages (master spec s10).
CREATE TABLE drafts (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    parent_id       TEXT,
    backend         TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    partial_text    TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;

-- What each assistant message was actually sent, for the inspector (s6.6).
CREATE TABLE context_views (
    message_id TEXT PRIMARY KEY NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    view_json  TEXT NOT NULL
) STRICT;

CREATE TABLE context_summaries (
    id                      TEXT PRIMARY KEY NOT NULL,
    conversation_id         TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    covers_from             TEXT NOT NULL,
    covers_to               TEXT NOT NULL,
    branch_leaf_at_creation TEXT NOT NULL,
    text                    TEXT NOT NULL,
    written_by_backend      TEXT NOT NULL,
    written_by_model        TEXT NOT NULL,
    prompt_version          INTEGER NOT NULL,
    token_estimate          INTEGER NOT NULL,
    created_at              TEXT NOT NULL
) STRICT;

CREATE INDEX idx_summaries_conversation ON context_summaries(conversation_id);

-- Model switches and similar. Rendered inline by the UI, never sent to a
-- model (master spec s5.3).
CREATE TABLE conversation_events (
    id                TEXT PRIMARY KEY NOT NULL,
    conversation_id   TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    after_message_id  TEXT,
    kind              TEXT NOT NULL,
    payload_json      TEXT NOT NULL,
    created_at        TEXT NOT NULL
) STRICT;

CREATE INDEX idx_events_conversation ON conversation_events(conversation_id, created_at);

-- Configured providers. Deliberately holds no secrets: API keys live only in
-- the OS credential store (master spec s12).
CREATE TABLE providers (
    id                 TEXT PRIMARY KEY NOT NULL,
    kind               TEXT NOT NULL,
    display_name       TEXT NOT NULL,
    base_url           TEXT,
    extra_headers_json TEXT,
    enabled            INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE model_profiles (
    backend      TEXT NOT NULL,
    model_id     TEXT NOT NULL,
    caps_json    TEXT NOT NULL,
    thinking_json TEXT,
    user_edited  INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (backend, model_id)
) STRICT;

-- GGUF files are registered by path and never copied (master spec s10).
CREATE TABLE local_models (
    id            TEXT PRIMARY KEY NOT NULL,
    path          TEXT NOT NULL UNIQUE,
    file_size     INTEGER NOT NULL,
    sha256        TEXT,
    architecture  TEXT,
    quant_summary TEXT,
    added_at      TEXT NOT NULL
) STRICT;

CREATE TABLE blobs (
    sha256     TEXT PRIMARY KEY NOT NULL,
    mime       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;
