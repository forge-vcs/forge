-- NER-136 Phase 5 tamper-evident evidence chain plus actor/identity model.
-- All additive. content_hash columns are nullable so pre-existing rows
-- grandfather (NULL means legacy_unverified). The integrity_marker high-water
-- mark below distinguishes a legacy NULL hash from a tampered one.
-- IMPORTANT the migration runner splits this file on the semicolon character,
-- so comments must never themselves contain a semicolon.
ALTER TABLE evidence ADD COLUMN content_hash TEXT;
ALTER TABLE evidence ADD COLUMN structured_json TEXT;
ALTER TABLE evidence ADD COLUMN actor TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE operations ADD COLUMN content_hash TEXT;
ALTER TABLE decisions ADD COLUMN content_hash TEXT;
ALTER TABLE decisions ADD COLUMN actor TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE attempts ADD COLUMN actor TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE publications ADD COLUMN actor TEXT NOT NULL DEFAULT 'unknown';

-- The legacy-vs-tampered discriminator. A NULL content_hash on a row whose rowid
-- is at or below the recorded high-water mark predates Phase 5 (grandfather). A
-- NULL hash on a row with rowid above the mark is a deleted hash (tamper). rowid is
-- monotonic and not assigned by normal INSERTs, so it raises the bar versus a
-- per-row created_at_ms timestamp (which an attacker can freely backdate). It is NOT
-- a hard barrier: the rowid of a TEXT-PRIMARY-KEY table is itself UPDATE-able, and
-- this marker row is outside the hash chain, so an actor with full DB write access
-- can still relabel a tampered row as legacy. That is the conceded tamper-EVIDENT
-- (not -PROOF) boundary closed by Phase 9 signing.
CREATE TABLE IF NOT EXISTS integrity_marker (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    evidence_high_water INTEGER NOT NULL,
    op_high_water INTEGER NOT NULL,
    decision_high_water INTEGER NOT NULL
);
INSERT OR IGNORE INTO integrity_marker (singleton, evidence_high_water, op_high_water, decision_high_water)
VALUES (
    1,
    (SELECT COALESCE(MAX(rowid), 0) FROM evidence),
    (SELECT COALESCE(MAX(rowid), 0) FROM operations),
    (SELECT COALESCE(MAX(rowid), 0) FROM decisions)
);
