CREATE TABLE IF NOT EXISTS storage_policy (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    protection_window_days INTEGER NOT NULL CHECK (protection_window_days >= 1),
    storage_budget_bytes INTEGER NOT NULL CHECK (storage_budget_bytes >= 0),
    automatic_eviction INTEGER NOT NULL DEFAULT 0 CHECK (automatic_eviction = 0)
);

INSERT OR IGNORE INTO storage_policy (
    singleton,
    protection_window_days,
    storage_budget_bytes,
    automatic_eviction
) VALUES (
    1,
    7,
    1073741824,
    0
);
