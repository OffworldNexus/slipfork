CREATE TABLE IF NOT EXISTS items (
    id bigserial PRIMARY KEY,
    name text NOT NULL,
    scope text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (scope, name)
);
