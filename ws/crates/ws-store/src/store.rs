//! Conversation and message persistence (master spec s10).

use crate::error::{Result, StoreError};
use crate::migrations;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;
use std::str::FromStr;
use ws_context::{
    BackendId, ContextSummary, ContextView, ConvId, Conversation, ConversationEvent, EventId,
    EventKind, Message, ModelRef, MsgId, MsgStatus, Part, Role, SummaryId,
};

/// A user-configured provider endpoint. Deliberately holds no secret; the key
/// lives in the OS credential store under the same id (master spec s12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProvider {
    pub id: BackendId,
    /// `openai_compat` today; the first-party adapters are built in.
    pub kind: String,
    pub display_name: String,
    pub base_url: Option<String>,
    pub extra_headers: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
}

/// A stored model profile, with its JSON left unparsed (master spec s7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProfile {
    pub model: ModelRef,
    pub caps_json: String,
    pub thinking_json: Option<String>,
    pub user_edited: bool,
}

/// A draft reply being streamed, checkpointed so a crash cannot lose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub conversation_id: ConvId,
    pub parent_id: Option<MsgId>,
    pub model: ModelRef,
    pub partial_text: String,
    pub updated_at: DateTime<Utc>,
}

/// A file an approved tool changed, with the exact bytes either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub call_id: String,
    pub conversation_id: ConvId,
    pub path: String,
    /// `None`: the file did not exist before.
    pub before_blob: Option<String>,
    /// `None`: the tool deleted it.
    pub after_blob: Option<String>,
    pub undone: bool,
    pub created_at: DateTime<Utc>,
}

/// A new message to persist. Separate from [`Message`] because the raw
/// provider payload is stored alongside the canonical parts but is not part of
/// the canonical format itself (master spec s5.2).
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub message: Message,
    pub raw_provider_payload: Option<serde_json::Value>,
}

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

fn role_to_sql(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_sql(s: &str) -> Result<Role> {
    match s {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(StoreError::BadEnum {
            kind: "role",
            value: other.to_owned(),
        }),
    }
}

fn status_to_sql(status: &MsgStatus) -> (&'static str, Option<&str>) {
    match status {
        MsgStatus::Complete => ("complete", None),
        MsgStatus::Cancelled => ("cancelled", None),
        MsgStatus::Failed { reason } => ("failed", Some(reason.as_str())),
    }
}

fn status_from_sql(tag: &str, reason: Option<String>) -> Result<MsgStatus> {
    match tag {
        "complete" => Ok(MsgStatus::Complete),
        "cancelled" => Ok(MsgStatus::Cancelled),
        "failed" => Ok(MsgStatus::Failed {
            reason: reason.unwrap_or_default(),
        }),
        other => Err(StoreError::BadEnum {
            kind: "status",
            value: other.to_owned(),
        }),
    }
}

fn parse_id<T: FromStr>(s: &str) -> Result<T> {
    T::from_str(s).map_err(|_| StoreError::BadId(s.to_owned()))
}

fn parse_opt_id<T: FromStr>(s: Option<String>) -> Result<Option<T>> {
    s.as_deref().map(parse_id).transpose()
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    ///
    /// # Errors
    /// Fails if the file cannot be opened or the schema cannot be migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure(conn)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    /// Fails if the schema cannot be created.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(conn)
    }

    fn configure(conn: Connection) -> Result<Self> {
        // WAL keeps readers from blocking the streaming writer. foreign_keys
        // is off by default in SQLite and must be enabled per connection, or
        // the ON DELETE CASCADE rules in the schema silently do nothing.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrations::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Escape hatch for callers that need raw SQL (tests, maintenance).
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // ---- conversations -------------------------------------------------

    /// # Errors
    /// Fails on any database error.
    pub fn create_conversation(&self, conv: &Conversation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversations
               (id, title, system_prompt, active_leaf_id, current_backend,
                current_model_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                conv.id.to_string(),
                conv.title,
                conv.system_prompt,
                conv.active_leaf_id.map(|id| id.to_string()),
                conv.current_model.backend.as_str(),
                conv.current_model.model_id,
                conv.created_at,
            ],
        )?;
        Ok(())
    }

    fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<StoredConv> {
        Ok(StoredConv {
            id: row.get("id")?,
            title: row.get("title")?,
            system_prompt: row.get("system_prompt")?,
            active_leaf_id: row.get("active_leaf_id")?,
            current_backend: row.get("current_backend")?,
            current_model_id: row.get("current_model_id")?,
            created_at: row.get("created_at")?,
        })
    }

    fn hydrate_conversation(r: StoredConv) -> Result<Conversation> {
        Ok(Conversation {
            id: parse_id(&r.id)?,
            title: r.title,
            system_prompt: r.system_prompt,
            active_leaf_id: parse_opt_id(r.active_leaf_id)?,
            current_model: ModelRef::new(r.current_backend, r.current_model_id),
            created_at: r.created_at,
        })
    }

    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn get_conversation(&self, id: ConvId) -> Result<Option<Conversation>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, title, system_prompt, active_leaf_id, current_backend,
                        current_model_id, created_at
                 FROM conversations WHERE id = ?1",
                params![id.to_string()],
                Self::conversation_from_row,
            )
            .optional()?;

        row.map(Self::hydrate_conversation).transpose()
    }

    /// Most recently updated first.
    ///
    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, system_prompt, active_leaf_id, current_backend,
                    current_model_id, created_at
             FROM conversations ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], Self::conversation_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::hydrate_conversation(row?)?);
        }
        Ok(out)
    }

    /// Move the visible thread to a different leaf (branch navigation).
    ///
    /// # Errors
    /// Fails on any database error.
    pub fn set_active_leaf(&self, conv: ConvId, leaf: Option<MsgId>) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET active_leaf_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![conv.to_string(), leaf.map(|l| l.to_string()), Utc::now()],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error.
    pub fn set_current_model(&self, conv: ConvId, model: &ModelRef) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations
             SET current_backend = ?2, current_model_id = ?3, updated_at = ?4
             WHERE id = ?1",
            params![
                conv.to_string(),
                model.backend.as_str(),
                model.model_id,
                Utc::now()
            ],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error.
    pub fn rename_conversation(&self, conv: ConvId, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![conv.to_string(), title, Utc::now()],
        )?;
        Ok(())
    }

    /// Delete a conversation and everything hanging off it. The only
    /// destructive operation in the product (master spec s5.1 invariant 5).
    ///
    /// # Errors
    /// Fails on any database error.
    pub fn delete_conversation(&self, conv: ConvId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM conversations WHERE id = ?1",
            params![conv.to_string()],
        )?;
        Ok(())
    }

    // ---- messages ------------------------------------------------------

    /// Append a message. Rows are inserted once and never rewritten.
    ///
    /// # Errors
    /// Fails on any database error, including an attempt to reuse an id.
    pub fn insert_message(&self, new: &NewMessage) -> Result<()> {
        let m = &new.message;
        let (status, reason) = status_to_sql(&m.status);
        let raw = new
            .raw_provider_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        self.conn.execute(
            "INSERT INTO messages
               (id, conversation_id, parent_id, role, author_backend, author_model_id,
                parts_json, raw_provider_payload, status, status_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                m.id.to_string(),
                m.conversation_id.to_string(),
                m.parent_id.map(|p| p.to_string()),
                role_to_sql(m.role),
                m.author_model.as_ref().map(|a| a.backend.as_str()),
                m.author_model.as_ref().map(|a| a.model_id.as_str()),
                serde_json::to_string(&m.parts)?,
                raw,
                status,
                reason,
                m.created_at,
            ],
        )?;

        self.conn.execute(
            "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
            params![m.conversation_id.to_string(), Utc::now()],
        )?;
        Ok(())
    }

    fn message_from_row(row: &Row<'_>) -> rusqlite::Result<StoredRow> {
        Ok(StoredRow {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            parent_id: row.get("parent_id")?,
            role: row.get("role")?,
            author_backend: row.get("author_backend")?,
            author_model_id: row.get("author_model_id")?,
            parts_json: row.get("parts_json")?,
            status: row.get("status")?,
            status_reason: row.get("status_reason")?,
            created_at: row.get("created_at")?,
        })
    }

    fn hydrate(r: StoredRow) -> Result<Message> {
        let parts: Vec<Part> = serde_json::from_str(&r.parts_json)?;
        Ok(Message {
            id: parse_id(&r.id)?,
            conversation_id: parse_id(&r.conversation_id)?,
            parent_id: parse_opt_id(r.parent_id)?,
            role: role_from_sql(&r.role)?,
            author_model: match (r.author_backend, r.author_model_id) {
                (Some(b), Some(m)) => Some(ModelRef::new(b, m)),
                _ => None,
            },
            parts,
            created_at: r.created_at,
            status: status_from_sql(&r.status, r.status_reason)?,
        })
    }

    /// # Errors
    /// Fails on any database error, or if stored content cannot be parsed.
    pub fn get_message(&self, id: MsgId) -> Result<Option<Message>> {
        let row = self
            .conn
            .query_row(
                "SELECT * FROM messages WHERE id = ?1",
                params![id.to_string()],
                Self::message_from_row,
            )
            .optional()?;
        row.map(Self::hydrate).transpose()
    }

    /// The raw provider response for a message, used for same-model replay.
    ///
    /// # Errors
    /// Fails on any database error, or if the stored payload is not JSON.
    pub fn get_raw_payload(&self, id: MsgId) -> Result<Option<serde_json::Value>> {
        let raw: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT raw_provider_payload FROM messages WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        match raw.flatten() {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// The thread from the root to `leaf`, in order.
    ///
    /// Walks parent links in one recursive query rather than one query per
    /// message, so a long conversation costs a single round trip.
    ///
    /// # Errors
    /// Fails on any database error, or if stored content cannot be parsed.
    pub fn load_path(&self, leaf: MsgId) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE ancestry(id, depth) AS (
                 SELECT id, 0 FROM messages WHERE id = ?1
                 UNION ALL
                 SELECT m.parent_id, ancestry.depth + 1
                 FROM messages m JOIN ancestry ON m.id = ancestry.id
                 WHERE m.parent_id IS NOT NULL
             )
             SELECT messages.* FROM messages
             JOIN ancestry ON messages.id = ancestry.id
             ORDER BY ancestry.depth DESC",
        )?;
        let rows = stmt.query_map(params![leaf.to_string()], Self::message_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::hydrate(row?)?);
        }
        Ok(out)
    }

    /// The thread currently shown for a conversation.
    ///
    /// # Errors
    /// Fails if the conversation does not exist, or on any database error.
    pub fn load_active_path(&self, conv: ConvId) -> Result<Vec<Message>> {
        let c = self
            .get_conversation(conv)?
            .ok_or_else(|| StoreError::ConversationNotFound(conv.to_string()))?;
        match c.active_leaf_id {
            Some(leaf) => self.load_path(leaf),
            None => Ok(Vec::new()),
        }
    }

    /// Every message sharing a parent with `id`, oldest first, `id` included.
    /// Drives the `< 2/3 >` branch navigator.
    ///
    /// # Errors
    /// Fails on any database error, or if stored content cannot be parsed.
    pub fn siblings(&self, id: MsgId) -> Result<Vec<Message>> {
        let Some(msg) = self.get_message(id)? else {
            return Ok(Vec::new());
        };
        // Ordered by rowid, which is SQLite's insertion order. Ordering by id
        // would be wrong: sibling replies are usually minted within the same
        // millisecond, and a ULID's sub-millisecond bits are random.
        let mut stmt = match msg.parent_id {
            Some(_) => self.conn.prepare(
                "SELECT * FROM messages
                 WHERE conversation_id = ?1 AND parent_id = ?2 ORDER BY rowid",
            )?,
            None => self.conn.prepare(
                "SELECT * FROM messages
                 WHERE conversation_id = ?1 AND parent_id IS NULL AND ?2 IS NULL
                 ORDER BY rowid",
            )?,
        };
        let rows = stmt.query_map(
            params![
                msg.conversation_id.to_string(),
                msg.parent_id.map(|p| p.to_string())
            ],
            Self::message_from_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::hydrate(row?)?);
        }
        Ok(out)
    }
}

/// Raw conversation columns, before ids are validated.
struct StoredConv {
    id: String,
    title: String,
    system_prompt: Option<String>,
    active_leaf_id: Option<String>,
    current_backend: String,
    current_model_id: String,
    created_at: DateTime<Utc>,
}

/// Raw message columns, before ids and enums are validated.
struct StoredRow {
    id: String,
    conversation_id: String,
    parent_id: Option<String>,
    role: String,
    author_backend: Option<String>,
    author_model_id: Option<String>,
    parts_json: String,
    status: String,
    status_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl Store {
    // ---- drafts --------------------------------------------------------

    /// Checkpoint an in-progress reply. Called about once a second while
    /// streaming, so it must be cheap and idempotent.
    ///
    /// # Errors
    /// Fails on any database error.
    /// # Errors
    /// Fails on any database error.
    pub fn save_file_edit(&self, e: &FileEdit) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_edits
               (call_id, conversation_id, path, before_blob, after_blob, undone, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                e.call_id,
                e.conversation_id.to_string(),
                e.path,
                e.before_blob,
                e.after_blob,
                e.undone,
                e.created_at,
            ],
        )?;
        Ok(())
    }

    /// Every edit in a conversation, oldest first.
    ///
    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn file_edits(&self, conv: ConvId) -> Result<Vec<FileEdit>> {
        self.query_edits(
            "WHERE conversation_id = ?1 ORDER BY rowid",
            &conv.to_string(),
        )
    }

    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn get_file_edit(&self, call_id: &str) -> Result<Option<FileEdit>> {
        Ok(self.query_edits("WHERE call_id = ?1", call_id)?.pop())
    }

    /// # Errors
    /// Fails on any database error.
    pub fn mark_edit_undone(&self, call_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE file_edits SET undone = 1 WHERE call_id = ?1",
            params![call_id],
        )?;
        Ok(())
    }

    fn query_edits(&self, filter: &str, arg: &str) -> Result<Vec<FileEdit>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT call_id, conversation_id, path, before_blob, after_blob, undone, created_at
             FROM file_edits {filter}"
        ))?;
        let rows = stmt.query_map(params![arg], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, DateTime<Utc>>(6)?,
            ))
        })?;
        rows.map(|r| {
            let (call_id, conv, path, before_blob, after_blob, undone, created_at) = r?;
            Ok(FileEdit {
                call_id,
                conversation_id: parse_id(&conv)?,
                path,
                before_blob,
                after_blob,
                undone,
                created_at,
            })
        })
        .collect()
    }

    pub fn save_draft(&self, draft: &Draft) -> Result<()> {
        self.conn.execute(
            "INSERT INTO drafts
               (conversation_id, parent_id, backend, model_id, partial_text, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(conversation_id) DO UPDATE SET
               parent_id = excluded.parent_id,
               backend = excluded.backend,
               model_id = excluded.model_id,
               partial_text = excluded.partial_text,
               updated_at = excluded.updated_at",
            params![
                draft.conversation_id.to_string(),
                draft.parent_id.map(|p| p.to_string()),
                draft.model.backend.as_str(),
                draft.model.model_id,
                draft.partial_text,
                draft.updated_at,
            ],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error.
    pub fn clear_draft(&self, conv: ConvId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM drafts WHERE conversation_id = ?1",
            params![conv.to_string()],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn list_drafts(&self) -> Result<Vec<Draft>> {
        let mut stmt = self.conn.prepare(
            "SELECT conversation_id, parent_id, backend, model_id, partial_text, updated_at
             FROM drafts",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, DateTime<Utc>>(5)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (conv, parent, backend, model, text, at) = row?;
            out.push(Draft {
                conversation_id: parse_id(&conv)?,
                parent_id: parse_opt_id(parent)?,
                model: ModelRef::new(backend, model),
                partial_text: text,
                updated_at: at,
            });
        }
        Ok(out)
    }

    /// Turn every leftover draft into a `Failed` message keeping its partial
    /// text, and clear the draft table. Run once at launch (master spec s10).
    ///
    /// Returns the ids of the messages created.
    ///
    /// # Errors
    /// Fails on any database error.
    pub fn recover_drafts(&self) -> Result<Vec<MsgId>> {
        let drafts = self.list_drafts()?;
        let mut created = Vec::new();

        for draft in drafts {
            let id = MsgId::new();
            let message = Message {
                id,
                conversation_id: draft.conversation_id,
                parent_id: draft.parent_id,
                role: Role::Assistant,
                author_model: Some(draft.model.clone()),
                parts: vec![Part::text(draft.partial_text.clone())],
                created_at: draft.updated_at,
                status: MsgStatus::Failed {
                    reason: "interrupted before the reply finished".to_owned(),
                },
            };
            self.insert_message(&NewMessage {
                message,
                raw_provider_payload: None,
            })?;
            // The recovered message becomes the visible leaf, so the partial
            // text is not hidden on a branch the user cannot see.
            self.set_active_leaf(draft.conversation_id, Some(id))?;
            self.clear_draft(draft.conversation_id)?;
            created.push(id);
        }
        Ok(created)
    }

    // ---- providers -----------------------------------------------------

    /// Save a user-configured provider. Holds no secret: the API key lives in
    /// the OS credential store under this same id (master spec s12).
    ///
    /// # Errors
    /// Fails on any database error, or if the headers cannot be serialised.
    pub fn save_provider(&self, p: &StoredProvider) -> Result<()> {
        self.conn.execute(
            "INSERT INTO providers (id, kind, display_name, base_url, extra_headers_json, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               kind = excluded.kind,
               display_name = excluded.display_name,
               base_url = excluded.base_url,
               extra_headers_json = excluded.extra_headers_json,
               enabled = excluded.enabled",
            params![
                p.id.as_str(),
                p.kind,
                p.display_name,
                p.base_url,
                serde_json::to_string(&p.extra_headers)?,
                i64::from(p.enabled),
            ],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error, or if stored JSON cannot be parsed.
    pub fn list_providers(&self) -> Result<Vec<StoredProvider>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, display_name, base_url, extra_headers_json, enabled
             FROM providers ORDER BY display_name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, kind, display_name, base_url, headers, enabled) = row?;
            out.push(StoredProvider {
                id: BackendId::new(id),
                kind,
                display_name,
                base_url,
                extra_headers: headers
                    .and_then(|h| serde_json::from_str(&h).ok())
                    .unwrap_or_default(),
                enabled: enabled != 0,
            });
        }
        Ok(out)
    }

    /// # Errors
    /// Fails on any database error.
    pub fn delete_provider(&self, id: &BackendId) -> Result<()> {
        self.conn
            .execute("DELETE FROM providers WHERE id = ?1", params![id.as_str()])?;
        Ok(())
    }

    // ---- model profiles ------------------------------------------------

    /// Save the user-editable settings for one model (master spec s7.1).
    ///
    /// # Errors
    /// Fails on any database error.
    pub fn save_model_profile(
        &self,
        model: &ModelRef,
        caps_json: &str,
        thinking_json: Option<&str>,
        user_edited: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO model_profiles
               (backend, model_id, caps_json, thinking_json, user_edited, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(backend, model_id) DO UPDATE SET
               caps_json = excluded.caps_json,
               thinking_json = excluded.thinking_json,
               user_edited = excluded.user_edited,
               updated_at = excluded.updated_at",
            params![
                model.backend.as_str(),
                model.model_id,
                caps_json,
                thinking_json,
                i64::from(user_edited),
                Utc::now(),
            ],
        )?;
        Ok(())
    }

    /// Every stored profile.
    ///
    /// JSON is returned unparsed so a row written by a newer build cannot fail
    /// startup; the caller decides what to do with something it cannot read.
    ///
    /// # Errors
    /// Fails on any database error.
    pub fn list_model_profiles(&self) -> Result<Vec<StoredProfile>> {
        let mut stmt = self.conn.prepare(
            "SELECT backend, model_id, caps_json, thinking_json, user_edited FROM model_profiles",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (backend, model_id, caps, thinking, edited) = row?;
            out.push(StoredProfile {
                model: ModelRef::new(backend, model_id),
                caps_json: caps,
                thinking_json: thinking,
                user_edited: edited != 0,
            });
        }
        Ok(out)
    }

    // ---- conversation events -------------------------------------------

    /// Record something that happened but is not a message: a model switch,
    /// say. Never sent to a model (master spec s5.3).
    ///
    /// # Errors
    /// Fails on any database error, or if the event cannot be serialised.
    pub fn record_event(
        &self,
        conv: ConvId,
        after: Option<MsgId>,
        kind: &EventKind,
    ) -> Result<EventId> {
        let id = EventId::new();
        // The tag is stored separately from the payload so events can be
        // filtered in SQL without parsing every row.
        let tag = match kind {
            EventKind::ModelSwitch { .. } => "model_switch",
        };
        self.conn.execute(
            "INSERT INTO conversation_events
               (id, conversation_id, after_message_id, kind, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.to_string(),
                conv.to_string(),
                after.map(|m| m.to_string()),
                tag,
                serde_json::to_string(kind)?,
                Utc::now(),
            ],
        )?;
        Ok(id)
    }

    /// Every event on a conversation, oldest first.
    ///
    /// # Errors
    /// Fails on any database error, or if a stored event cannot be parsed.
    pub fn list_events(&self, conv: ConvId) -> Result<Vec<ConversationEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, after_message_id, payload_json, created_at
             FROM conversation_events WHERE conversation_id = ?1
             ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map(params![conv.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, DateTime<Utc>>(3)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, after, payload, at) = row?;
            out.push(ConversationEvent {
                id: parse_id(&id)?,
                conversation_id: conv,
                after_message_id: parse_opt_id(after)?,
                kind: serde_json::from_str(&payload)?,
                at,
            });
        }
        Ok(out)
    }

    /// The model that wrote the most recent assistant reply on the active
    /// path, when it is not the model the conversation now points at.
    ///
    /// This is the model being switched away from, and master spec s6.5
    /// prefers it as summariser because it still holds the context being
    /// compressed. Defining it by authorship rather than by a switch event is
    /// what makes it self-invalidating: as soon as a reply is generated on the
    /// new model, the last author *is* the current model and this returns
    /// `None`. Keying it on the leaf pointer instead would expire it the
    /// moment the user typed their next question, which is too early.
    ///
    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn outgoing_model(&self, conv: ConvId) -> Result<Option<ModelRef>> {
        let Some(current) = self.get_conversation(conv)? else {
            return Ok(None);
        };
        let path = self.load_active_path(conv)?;

        let last_author = path
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| m.author_model.clone());

        Ok(match last_author {
            Some(author) if author != current.current_model => Some(author),
            _ => None,
        })
    }

    // ---- context views and summaries -----------------------------------

    /// # Errors
    /// Fails on any database error, or if the view cannot be serialised.
    pub fn save_context_view(&self, message: MsgId, view: &ContextView) -> Result<()> {
        self.conn.execute(
            "INSERT INTO context_views (message_id, view_json) VALUES (?1, ?2)
             ON CONFLICT(message_id) DO UPDATE SET view_json = excluded.view_json",
            params![message.to_string(), serde_json::to_string(view)?],
        )?;
        Ok(())
    }

    /// # Errors
    /// Fails on any database error, or if the stored view cannot be parsed.
    pub fn get_context_view(&self, message: MsgId) -> Result<Option<ContextView>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT view_json FROM context_views WHERE message_id = ?1",
                params![message.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        match json {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// # Errors
    /// Fails on any database error.
    pub fn save_summary(&self, s: &ContextSummary) -> Result<()> {
        self.conn.execute(
            "INSERT INTO context_summaries
               (id, conversation_id, covers_from, covers_to, branch_leaf_at_creation,
                text, written_by_backend, written_by_model, prompt_version,
                token_estimate, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                s.id.to_string(),
                s.conversation_id.to_string(),
                s.covers_from.to_string(),
                s.covers_to.to_string(),
                s.branch_leaf_at_creation.to_string(),
                s.text,
                s.written_by.backend.as_str(),
                s.written_by.model_id,
                s.prompt_version,
                s.token_estimate,
                s.created_at,
            ],
        )?;
        Ok(())
    }

    /// Every summary recorded for a conversation. The context builder decides
    /// which of them are valid for the current path.
    ///
    /// # Errors
    /// Fails on any database error, or if a stored id is malformed.
    pub fn list_summaries(&self, conv: ConvId) -> Result<Vec<ContextSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, conversation_id, covers_from, covers_to, branch_leaf_at_creation,
                    text, written_by_backend, written_by_model, prompt_version,
                    token_estimate, created_at
             FROM context_summaries WHERE conversation_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![conv.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, u32>(8)?,
                row.get::<_, u32>(9)?,
                row.get::<_, DateTime<Utc>>(10)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, conv_id, from, to, leaf, text, backend, model, version, tokens, at) = row?;
            out.push(ContextSummary {
                id: parse_id::<SummaryId>(&id)?,
                conversation_id: parse_id(&conv_id)?,
                covers_from: parse_id(&from)?,
                covers_to: parse_id(&to)?,
                branch_leaf_at_creation: parse_id(&leaf)?,
                text,
                written_by: ModelRef::new(backend, model),
                prompt_version: version,
                token_estimate: tokens,
                created_at: at,
            });
        }
        Ok(out)
    }
}
