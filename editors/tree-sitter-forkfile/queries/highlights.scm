; Highlights for `# forkfile: 0.1`.

[
  "features"
  "default"
  "image"
  "from"
  "dockerfile"
  "stage"
  "workdir"
  "arg"
  "use"
  "env"
  "before-start"
  "pre-deploy"
  "post-deploy"
  "failed-deploy"
  "run"
  "when"
  "changed"
  "service"
  "port"
  "internal_ports"
  "route"
  "strip-prefix"
  "rewrite"
  "exact"
  "domain"
  "postgres"
  "database"
  "version"
  "redis"
  "s3"
  "cdn"
  "policy"
  "on"
] @keyword

(header) @preproc
(comment) @comment
(boolean) @boolean
(integer) @number
(string) @string

(tool) @function
(command_text) @string.special
(port_reference) @variable.builtin
(env_reference) @variable.builtin
(route_path) @string.special
(domain_prefix) @string.special

[
  "var"
  "secret"
  "random_hex"
  "random_base64"
  "random_alnum"
  "random_uuid"
] @function.builtin

(property property: (identifier) @property)
(property base: (identifier) @variable)
(env_entry key: (identifier) @property)
(arg_item name: (identifier) @property)
(arg_entry name: (identifier) @property)
(image id: (identifier) @type)
(service id: (identifier) @type)
(postgres id: (identifier) @type)
(database_declaration id: (identifier) @type)
(redis id: (identifier) @type)
(s3 bucket: (identifier) @type)
(cdn_declaration id: (identifier) @type)
(domain_declaration id: (identifier) @type)

[
  "|"
  "&"
  "!"
  "+"
  "??"
  "="
  ":"
] @operator

(identifier) @variable
