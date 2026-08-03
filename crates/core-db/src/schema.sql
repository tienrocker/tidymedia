-- media-dedup index schema v1
-- All timestamps are unix epoch milliseconds unless noted.

CREATE TABLE volumes(
  id INTEGER PRIMARY KEY,
  guid TEXT UNIQUE,              -- \\?\Volume{...}\ khi có; tạm thời "D:" ở M1
  letter TEXT,
  serial INTEGER,
  fs TEXT,
  supports_usn INTEGER NOT NULL DEFAULT 0,
  usn_journal_id INTEGER,
  last_usn INTEGER,
  added_at INTEGER
);

CREATE TABLE roots(
  id INTEGER PRIMARY KEY,
  volume_id INTEGER NOT NULL REFERENCES volumes(id),
  path TEXT NOT NULL UNIQUE,
  last_scan_at INTEGER,
  scan_state TEXT
);

CREATE TABLE dirs(
  id INTEGER PRIMARY KEY,
  volume_id INTEGER NOT NULL,
  frn INTEGER,
  parent_id INTEGER,
  name TEXT NOT NULL,
  path TEXT NOT NULL,            -- materialized full path, no trailing sep (except drive root "D:\")
  UNIQUE(volume_id, path)
);
CREATE INDEX dirs_frn ON dirs(volume_id, frn) WHERE frn IS NOT NULL;
CREATE INDEX dirs_path ON dirs(path);

CREATE TABLE files(
  id INTEGER PRIMARY KEY,
  dir_id INTEGER NOT NULL REFERENCES dirs(id),
  volume_id INTEGER NOT NULL,
  frn INTEGER,                   -- NTFS FileReferenceNumber, ổn định qua move/rename
  name TEXT NOT NULL,
  original_name TEXT,            -- tên trước khi organize
  ext TEXT,                      -- lowercase, không dấu chấm
  kind INTEGER NOT NULL,         -- 0=image 1=video
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  ctime INTEGER,
  attrs INTEGER,
  status INTEGER NOT NULL DEFAULT 0,   -- 0 present, 1 missing, 2 cloud placeholder, 3 missing-volume
  corrupt INTEGER,               -- NULL unknown, 0 ok, 1 decode failed
  rating INTEGER NOT NULL DEFAULT 0,
  live_pair_id INTEGER,          -- id file MOV của Live Photo
  seen_gen INTEGER NOT NULL DEFAULT 0, -- scan generation reconciliation
  UNIQUE(dir_id, name)
);
CREATE INDEX files_vol_frn ON files(volume_id, frn) WHERE frn IS NOT NULL;
CREATE INDEX files_size ON files(size);
CREATE INDEX files_ext ON files(ext);
CREATE INDEX files_mtime ON files(mtime);
CREATE INDEX files_kind_mtime ON files(kind, mtime);
CREATE INDEX files_name_nc ON files(name COLLATE NOCASE);
CREATE INDEX files_seen ON files(seen_gen);

CREATE VIRTUAL TABLE files_fts USING fts5(
  name,
  content='files',
  content_rowid='id',
  tokenize='trigram'
);
CREATE TRIGGER files_fts_ai AFTER INSERT ON files BEGIN
  INSERT INTO files_fts(rowid, name) VALUES (new.id, new.name);
END;
CREATE TRIGGER files_fts_ad AFTER DELETE ON files BEGIN
  INSERT INTO files_fts(files_fts, rowid, name) VALUES ('delete', old.id, old.name);
END;
CREATE TRIGGER files_fts_au AFTER UPDATE OF name ON files BEGIN
  INSERT INTO files_fts(files_fts, rowid, name) VALUES ('delete', old.id, old.name);
  INSERT INTO files_fts(rowid, name) VALUES (new.id, new.name);
END;

CREATE TABLE media_meta(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  width INTEGER,
  height INTEGER,
  duration_ms INTEGER,
  vcodec TEXT,
  acodec TEXT,
  bitrate INTEGER,
  fps REAL,
  taken_at INTEGER,
  date_source INTEGER,           -- 0=EXIF 1=QuickTime 2=filename-parse 3=mtime-uncertain
  camera TEXT,
  orientation INTEGER,
  meta_state INTEGER NOT NULL DEFAULT 0  -- 0 pending 1 done 2 failed
);
CREATE INDEX meta_dims ON media_meta(width, height);
CREATE INDEX meta_dur ON media_meta(duration_ms);
CREATE INDEX meta_taken ON media_meta(taken_at);

CREATE TABLE hashes(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  quick64 INTEGER,               -- xxh3(first 4KB ++ last 4KB ++ size)
  full_hash BLOB,                -- blake3 32 bytes
  hashed_size INTEGER,
  hashed_mtime INTEGER
);
CREATE INDEX hashes_quick ON hashes(quick64);
CREATE INDEX hashes_full ON hashes(full_hash) WHERE full_hash IS NOT NULL;

CREATE TABLE phashes(
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  kind INTEGER NOT NULL,         -- 0 image dhash, 1 image phash, 2 video keyframe
  seq INTEGER NOT NULL DEFAULT 0,
  hash64 INTEGER NOT NULL,
  PRIMARY KEY(file_id, kind, seq)
) WITHOUT ROWID;

CREATE TABLE dup_groups(
  id INTEGER PRIMARY KEY,
  kind INTEGER NOT NULL,         -- 0 exact, 1 similar-image, 2 similar-video
  score REAL,
  created_at INTEGER,
  resolved_at INTEGER
);
CREATE TABLE dup_members(
  group_id INTEGER NOT NULL REFERENCES dup_groups(id) ON DELETE CASCADE,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  keep INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(group_id, file_id)
) WITHOUT ROWID;

CREATE TABLE tags(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE file_tags(
  file_id INTEGER NOT NULL,
  tag_id INTEGER NOT NULL,
  PRIMARY KEY(file_id, tag_id)
) WITHOUT ROWID;
CREATE TABLE albums(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER);
CREATE TABLE album_files(
  album_id INTEGER NOT NULL,
  file_id INTEGER NOT NULL,
  pos INTEGER,
  PRIMARY KEY(album_id, file_id)
) WITHOUT ROWID;

CREATE TABLE library_roots(
  id INTEGER PRIMARY KEY,
  volume_id INTEGER NOT NULL UNIQUE REFERENCES volumes(id),
  path TEXT NOT NULL,
  is_primary INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE imports(
  id INTEGER PRIMARY KEY,
  source_kind TEXT NOT NULL,     -- 'folder' | 'mtp'
  device_id TEXT,
  started_at INTEGER,
  finished_at INTEGER,
  total INTEGER,
  copied INTEGER,
  skipped_seen INTEGER,
  skipped_dupe INTEGER,
  error TEXT
);
CREATE TABLE import_seen(
  device_id TEXT NOT NULL,
  name TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  imported_at INTEGER,
  PRIMARY KEY(device_id, name, size, mtime)
) WITHOUT ROWID;

CREATE TABLE org_ops(
  id INTEGER PRIMARY KEY,
  batch_id INTEGER NOT NULL,
  file_id INTEGER NOT NULL,
  old_path TEXT NOT NULL,
  new_path TEXT NOT NULL,
  done_at INTEGER
);
CREATE INDEX org_ops_batch ON org_ops(batch_id);

CREATE TABLE jobs(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,           -- queued | running | done | failed | cancelled
  params TEXT,
  done INTEGER NOT NULL DEFAULT 0,
  total INTEGER,
  message TEXT,
  created_at INTEGER,
  started_at INTEGER,
  finished_at INTEGER,
  error TEXT
);

CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT);
