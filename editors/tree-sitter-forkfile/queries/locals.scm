; Locals for `# forkfile: 0.1`.

(image) @local.scope
(service) @local.scope
(postgres) @local.scope
(redis) @local.scope
(s3) @local.scope
(env) @local.scope
(env_block) @local.scope

(image id: (identifier) @local.definition)
(service id: (identifier) @local.definition)
(postgres id: (identifier) @local.definition)
(redis id: (identifier) @local.definition)
(s3 bucket: (identifier) @local.definition)
(domain_declaration id: (identifier) @local.definition)
(database_declaration id: (identifier) @local.definition)
(cdn_declaration id: (identifier) @local.definition)
(env group: (identifier) @local.definition)
(env_entry key: (identifier) @local.definition)

(identifier) @local.reference
