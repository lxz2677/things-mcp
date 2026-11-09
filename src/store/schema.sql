-- Things Cloud local replica.
--
-- Queryable columns are modelled explicitly; `raw` keeps every wire property
-- ever seen for the entity so unknown/new fields are never lost.

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS task (
  id                            TEXT PRIMARY KEY,
  kind                          TEXT NOT NULL DEFAULT '',
  "type"                        INTEGER NOT NULL DEFAULT 0,
  status                        INTEGER NOT NULL DEFAULT 0,
  trashed                       INTEGER NOT NULL DEFAULT 0,
  title                         TEXT NOT NULL DEFAULT '',
  notes                         TEXT NOT NULL DEFAULT '',

  creation_date                 REAL,
  user_modification_date        REAL,
  stop_date                     REAL,
  last_reminder_interaction_date REAL,

  start                         INTEGER NOT NULL DEFAULT 0,
  start_date                    INTEGER,
  start_bucket                  INTEGER NOT NULL DEFAULT 0,
  deadline                      INTEGER,
  deadline_suppression_date     INTEGER,
  deadline_offset               INTEGER NOT NULL DEFAULT 0,
  reminder_time                 INTEGER,

  index_value                   INTEGER,
  today_index                   INTEGER,
  today_index_reference_date    INTEGER,

  project                       TEXT,
  area                          TEXT,
  heading                       TEXT,

  recurrence_rule               TEXT,
  repeating_template            TEXT,
  instance_creation_start_date  INTEGER,
  instance_creation_paused      INTEGER NOT NULL DEFAULT 0,
  instance_creation_count       INTEGER NOT NULL DEFAULT 0,
  after_completion_reference_date INTEGER,
  next_instance_start_date      INTEGER,
  leaves_tombstone              INTEGER NOT NULL DEFAULT 0,
  experimental                  TEXT,
  raw                           TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_task_project ON task(project);
CREATE INDEX IF NOT EXISTS idx_task_area ON task(area);
CREATE INDEX IF NOT EXISTS idx_task_heading ON task(heading);
CREATE INDEX IF NOT EXISTS idx_task_type ON task("type");
CREATE INDEX IF NOT EXISTS idx_task_status ON task(status);
CREATE INDEX IF NOT EXISTS idx_task_start ON task(start);
CREATE INDEX IF NOT EXISTS idx_task_start_date ON task(start_date);
CREATE INDEX IF NOT EXISTS idx_task_today_ref ON task(today_index_reference_date);
CREATE INDEX IF NOT EXISTS idx_task_deadline ON task(deadline);
CREATE INDEX IF NOT EXISTS idx_task_stop ON task(stop_date);
CREATE INDEX IF NOT EXISTS idx_task_trashed ON task(trashed);

CREATE TABLE IF NOT EXISTS area (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL DEFAULT '',
  title       TEXT NOT NULL DEFAULT '',
  visible     INTEGER NOT NULL DEFAULT 1,
  index_value INTEGER,
  raw         TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS tag (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL DEFAULT '',
  title       TEXT NOT NULL DEFAULT '',
  shortcut    TEXT,
  parent      TEXT,
  index_value INTEGER,
  raw         TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_tag_parent ON tag(parent);

CREATE TABLE IF NOT EXISTS checklist_item (
  id                    TEXT PRIMARY KEY,
  kind                  TEXT NOT NULL DEFAULT '',
  title                 TEXT NOT NULL DEFAULT '',
  status                INTEGER NOT NULL DEFAULT 0,
  index_value           INTEGER,
  creation_date         REAL,
  user_modification_date REAL,
  stop_date             REAL,
  task                  TEXT,
  leaves_tombstone      INTEGER NOT NULL DEFAULT 0,
  raw                   TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_checklist_task ON checklist_item(task);

CREATE TABLE IF NOT EXISTS task_tag (
  task TEXT NOT NULL,
  tag  TEXT NOT NULL,
  PRIMARY KEY (task, tag)
);
CREATE INDEX IF NOT EXISTS idx_task_tag_tag ON task_tag(tag);

CREATE TABLE IF NOT EXISTS area_tag (
  area TEXT NOT NULL,
  tag  TEXT NOT NULL,
  PRIMARY KEY (area, tag)
);
