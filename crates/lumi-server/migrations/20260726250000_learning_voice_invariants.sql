-- Lumi 0.3.0/E4 owner, session-item and retry-safety invariants.

ALTER TABLE audio_attachments
    ADD CONSTRAINT audio_attachments_owner_id_id_key UNIQUE (owner_id, id);

ALTER TABLE learning_attachment_refs
    ADD CONSTRAINT learning_attachment_refs_owner_attachment_fk
    FOREIGN KEY (owner_id, attachment_id)
    REFERENCES audio_attachments(owner_id, id)
    ON DELETE CASCADE;

ALTER TABLE learning_attachment_refs
    ADD CONSTRAINT learning_attachment_refs_session_item_fk
    FOREIGN KEY (session_id, item_id)
    REFERENCES learning_session_items(session_id, item_id)
    ON DELETE CASCADE;

ALTER TABLE transcript_artifacts
    ADD CONSTRAINT transcript_artifacts_owner_attachment_fk
    FOREIGN KEY (owner_id, attachment_id)
    REFERENCES audio_attachments(owner_id, id)
    ON DELETE CASCADE;
