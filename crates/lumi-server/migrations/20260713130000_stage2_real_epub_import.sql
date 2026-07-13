-- S1 Stage 2 durable real EPUB import, blobs and recoverable jobs.

ALTER TABLE document_revisions
    ADD COLUMN normalized_hash text,
    ADD COLUMN package_format_version text,
    ADD COLUMN source_blob_hash text REFERENCES blobs(content_hash),
    ADD COLUMN supersedes_revision_id uuid REFERENCES document_revisions(revision_id);

ALTER TABLE normalized_packages
    ADD COLUMN manifest_id uuid REFERENCES blob_manifests(manifest_id),
    ADD COLUMN package_blob_hash text REFERENCES blobs(content_hash);

ALTER TABLE import_jobs
    ADD COLUMN revision_id uuid REFERENCES document_revisions(revision_id),
    ADD COLUMN idempotency_key text,
    ADD COLUMN attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    ADD COLUMN max_attempts integer NOT NULL DEFAULT 3 CHECK (max_attempts BETWEEN 1 AND 10),
    ADD COLUMN cancellation_requested boolean NOT NULL DEFAULT false,
    ADD COLUMN error_code text,
    ADD COLUMN started_at timestamptz,
    ADD COLUMN finished_at timestamptz;

CREATE UNIQUE INDEX import_jobs_space_idempotency_idx
    ON import_jobs(space_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX import_jobs_recovery_idx
    ON import_jobs(status, created_at)
    WHERE status IN ('queued', 'running');

ALTER TABLE materials
    ADD COLUMN import_status text NOT NULL DEFAULT 'ready'
        CHECK (import_status IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    ADD COLUMN latest_import_job_id uuid REFERENCES import_jobs(job_id);

ALTER TABLE import_diagnostics
    ADD COLUMN attempt integer NOT NULL DEFAULT 1 CHECK (attempt > 0),
    ADD COLUMN details jsonb NOT NULL DEFAULT '{}'::jsonb;

CREATE INDEX import_diagnostics_job_attempt_idx
    ON import_diagnostics(job_id, attempt, diagnostic_id);
