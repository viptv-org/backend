-- One scalar per account; mutations and reads share the SQLite snapshot.
CREATE TABLE IF NOT EXISTS provider_match_revisions_v2(account_id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS provider_vod_match_revision_v2_insert
AFTER INSERT ON provider_vod
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=new.provider_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_vod_match_revision_v2_update
AFTER UPDATE ON provider_vod
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=old.provider_id UNION SELECT account_id FROM provider_ownership WHERE provider_id=new.provider_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_vod_match_revision_v2_delete
AFTER DELETE ON provider_vod
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=old.provider_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_matches_match_revision_v2_insert
AFTER INSERT ON provider_matches
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT o.account_id FROM provider_ownership o JOIN provider_vod v ON v.provider_id=o.provider_id WHERE v.id=new.vod_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_matches_match_revision_v2_update
AFTER UPDATE ON provider_matches
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT o.account_id FROM provider_ownership o JOIN provider_vod v ON v.provider_id=o.provider_id WHERE v.id=old.vod_id UNION SELECT o.account_id FROM provider_ownership o JOIN provider_vod v ON v.provider_id=o.provider_id WHERE v.id=new.vod_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_matches_match_revision_v2_delete
AFTER DELETE ON provider_matches
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT o.account_id FROM provider_ownership o JOIN provider_vod v ON v.provider_id=o.provider_id WHERE v.id=old.vod_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_ownership_match_revision_v2_insert
AFTER INSERT ON provider_ownership
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT new.account_id AS account_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_ownership_match_revision_v2_update
AFTER UPDATE ON provider_ownership
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT old.account_id AS account_id UNION SELECT new.account_id AS account_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS provider_ownership_match_revision_v2_delete
AFTER DELETE ON provider_ownership
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT old.account_id AS account_id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS providers_match_revision_v2_insert
AFTER INSERT ON providers
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=new.id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS providers_match_revision_v2_update
AFTER UPDATE OF id,enabled,enable_movies,enable_series ON providers
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=old.id UNION SELECT account_id FROM provider_ownership WHERE provider_id=new.id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
CREATE TRIGGER IF NOT EXISTS providers_match_revision_v2_delete
AFTER DELETE ON providers
BEGIN
  INSERT INTO provider_match_revisions_v2(account_id,revision) SELECT account_id,1 FROM (SELECT account_id FROM provider_ownership WHERE provider_id=old.id) WHERE true
  ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;
END;
