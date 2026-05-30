-- NER-138 Phase 7 slice 3: justified commit-on-accept + navigable native history.
--
-- decisions.commit_id records the native Commit written when a proposal is accepted in a
-- native repo (NULL for git-backend repos and for pre-006 decisions). It is the
-- ledger-authoritative anchor the ref-store HEAD reconciles against after a crash.
--
-- Two DISTINCT native-object format concerns are recorded as separate registry columns so
-- a future reader can tell them apart (slice-3 doc-review finding -- do not conflate):
--   commit_schema_version: the commit PAYLOAD feature epoch. Slice 3 introduces justified
--     commits that may carry actor + authored_time in their hashed bytes (Phase 9 signs
--     who/when). The per-object CommitObject.schema_version field stays 1 for hash
--     stability -- genesis commits must hash identically to slice 2 -- so this registry
--     value is a store-level capability marker, NOT a per-object parser key.
--   object_format_version: the on-disk object FRAMING epoch. Slice 3 stores each object as
--     its self-describing domain-separated preimage (kind in a header) so the gc/doctor
--     scan reads the kind instead of re-hashing under every kind. Legacy headerless objects
--     stay readable via a probe fallback, so this is additive, not a rewrite.
-- NOTE: the migration applier splits naively on the statement terminator, so neither the
-- DDL nor these comments may contain that terminator inside text.
ALTER TABLE decisions ADD COLUMN commit_id TEXT;

ALTER TABLE native_object_format ADD COLUMN object_format_version INTEGER NOT NULL DEFAULT 1;

UPDATE native_object_format SET commit_schema_version = 2, object_format_version = 2 WHERE singleton = 1;
