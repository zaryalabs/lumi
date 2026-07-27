ALTER TABLE import_jobs
    DROP CONSTRAINT import_jobs_source_kind_check;

ALTER TABLE import_jobs
    ADD CONSTRAINT import_jobs_source_kind_check
    CHECK (source_kind IN ('epub', 'pdf', 'web_page', 'telegram', 'markdown'));

CREATE INDEX import_jobs_active_markdown_idx
    ON import_jobs(status, created_at)
    WHERE source_kind = 'markdown' AND status IN ('queued', 'running');
