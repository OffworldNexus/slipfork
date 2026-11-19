; Statement keywords
[
  "project" "features" "postgres" "redis" "s3" "domain" "env" "service"
  "task" "on" "from" "in" "use" "database" "version"
] @keyword

; Directive keywords
[
  "port" "route" "internal_ports" "volume" "dockerfile" "stage" "size"
  "build_env" "run"
] @keyword

; Lifecycle hooks are commands
[
  "setup" "pre-deploy" "post-deploy" "failed-deploy"
] @function

(comment) @comment
(boolean) @boolean
(integer) @number
(string) @string
(raw_string) @string.special
(reference) @variable.special
(reference_path (identifier) @variable)
(function_call (identifier) @function)
(path) @string.special
(route_suffix) @string.special
(image_ref) @string.special
(flag) @attribute

(project_statement name: (identifier) @type)
(postgres_statement name: (identifier) @type)
(redis_statement name: (identifier) @type)
(s3_statement name: (identifier) @type)
(domain_statement name: (identifier) @type)
(service_statement name: (identifier) @type)
(database_statement name: (identifier) @type)
(env_group name: (identifier) @label)
(task_statement name: (identifier) @label)
(condition_identifier) @constant
(feature_declaration name: (identifier) @constant)
(assignment key: (identifier) @property)
(env_assignment key: (identifier) @property)
(env_key_assignment key: (identifier) @property)
(use_env_statement group: (identifier) @constant)

["+" "??" "&" "|"] @operator
"!" @operator
["{" "}" "[" "]" "(" ")"] @punctuation.bracket
["," ":"] @punctuation.delimiter
