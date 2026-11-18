INSERT INTO items (name, scope) VALUES
    ('alpha', 'shared'),
    ('beta', 'shared'),
    ('gamma', 'shared')
ON CONFLICT (scope, name) DO NOTHING;
