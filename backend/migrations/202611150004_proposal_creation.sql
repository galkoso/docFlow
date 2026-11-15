-- Initial mode is a navigation preference, never a restriction on later editing.
ALTER TABLE proposals ADD COLUMN initial_mode text NOT NULL DEFAULT 'manual'
    CHECK (initial_mode IN ('manual','ai'));
ALTER TABLE proposals ADD COLUMN links jsonb NOT NULL DEFAULT '[]'
    CHECK (jsonb_typeof(links)='array' AND jsonb_array_length(links)<=20);

-- One creation journal and job per permanently reserved proposal identity.
CREATE UNIQUE INDEX proposal_creation_operation ON git_operations(proposal_id)
    WHERE kind='CREATE';
CREATE UNIQUE INDEX proposal_creation_job ON jobs(operation_id)
    WHERE kind='CREATE_PROPOSAL';
