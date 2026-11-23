/**
 * Tree-sitter grammar for `# forkfile: 0.1`.
 *
 * Mirrors `specs/01-forkfile.md`: header, features, image/arg, env/secret,
 * service/port/route/domain, hooks with @tool prequels, `on ... else ...`
 * guards, `"${...}"` interpolation, and backing resources.
 */
module.exports = grammar({
  name: 'forkfile',

  extras: $ => [/\s/, $.comment],

  word: $ => $.identifier,

  conflicts: $ => [
    [$.on_statement],
    [$.on_image_item],
    [$.on_service_item],
    [$.on_postgres_item],
    [$.on_redis_item],
    [$.on_s3_item],
    [$.on_env_item],
    [$.on_command_item],
  ],

  rules: {
    source_file: $ => seq($.header, repeat($._declaration)),

    header: $ => token(prec(10, /#[ \t]*forkfile:[ \t]*[0-9]+\.[0-9]+/)),
    comment: $ => token(prec(-1, /#[^\n]*/)),

    _declaration: $ => choice(
      $.features,
      $.image,
      $.on_statement,
      $.domain_declaration,
      $.postgres,
      $.redis,
      $.s3,
      $.env,
    ),

    features: $ => seq('features', '{', repeat($.feature_group), '}'),
    feature_group: $ => choice(
      seq('default', '=', '[', commaSep($.identifier), ']'),
      $.identifier,
    ),

    image: $ => seq(
      'image',
      field('id', $.identifier),
      field('source', $.image_source),
      '{',
      repeat($._image_item),
      '}',
    ),
    image_source: $ => choice($.image_from, $.directory),
    image_from: $ => seq('from', field('reference', $.image_reference)),
    image_reference: $ => token(/[A-Za-z0-9_.\/:@-]+/),
    directory: $ => token(prec(1, /(?:\.{1,2})(?:\/[A-Za-z0-9._-]+)*|[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._-]+)+/)),

    _image_item: $ => choice(
      $.dockerfile,
      $.stage,
      $.workdir,
      $.arg_item,
      $.use_env,
      $.env_block,
      $.service,
      $.hook,
      $.on_image_item,
    ),

    dockerfile: $ => seq('dockerfile', $.path),
    stage: $ => seq('stage', $.identifier),
    workdir: $ => seq('workdir', $.path),
    path: $ => token(/[A-Za-z0-9_.\/-]+/),

    arg_item: $ => seq(
      'arg',
      choice(
        seq('{', repeat($.arg_entry), '}'),
        seq(
          field('name', $.identifier),
          '=',
          field('value', $._expression),
        ),
      ),
    ),
    arg_entry: $ => seq(
      field('name', $.identifier),
      '=',
      field('value', $._expression),
    ),
    use_env: $ => seq('use', 'env', field('group', $.identifier)),

    env: $ => seq('env', field('group', $.identifier), '{', repeat($._env_item), '}'),
    env_block: $ => seq('env', '{', repeat($._env_item), '}'),
    _env_item: $ => choice($.env_entry, $.on_env_item),
    env_entry: $ => seq(
      field('key', $.identifier),
      '=',
      field('value', $._expression),
    ),

    service: $ => seq(
      'service',
      field('id', $.identifier),
      '{',
      repeat($._service_item),
      '}',
    ),
    _service_item: $ => choice(
      $.port,
      $.internal_ports,
      $.route,
      $.hook,
      $.on_service_item,
    ),
    port: $ => seq('port', $.integer),
    internal_ports: $ => seq('internal_ports', '[', commaSep($.integer), ']'),

    route: $ => seq(
      'route',
      optional(field('path', $.route_path)),
      repeat($.route_modifier),
    ),
    route_path: $ => token(/\/(?:[A-Za-z0-9._~\/-]*)/),
    route_modifier: $ => choice(
      'strip-prefix',
      seq('rewrite', field('target', $.route_path)),
      seq('exact', field('target', $.route_path)),
      $.domain_clause,
    ),
    domain_clause: $ => seq('domain', field('filter', $.domain_filter)),
    domain_filter: $ => $.identifier,

    hook: $ => seq(
      field('name', choice('before-start', 'pre-deploy', 'post-deploy', 'failed-deploy', 'run')),
      optional($.change_guard),
      choice(seq(':', $.command), $.command_block),
    ),
    change_guard: $ => seq('when', 'changed', '(', commaSep($.path), ')'),
    command_block: $ => seq('{', repeat($._command_item), '}'),
    _command_item: $ => choice($.command, $.on_command_item),
    on_command_item: $ => choice(
      prec.dynamic(
        1,
        seq(
          // `on` outranks `command_text` so a guard line is never swallowed as a shell line.
          token(prec(2, 'on')),
          field('condition', $._expression),
          $._on_command_body,
          $._else_command_item,
        ),
      ),
      seq(
        token(prec(2, 'on')),
        field('condition', $._expression),
        $._on_command_body,
      ),
    ),
    _on_command_body: $ => choice(
      seq(':', $._command_item),
      $.command_block,
    ),
    _else_command_item: $ => seq(
      token(prec(2, 'else')),
      choice(
        seq(':', $._command_item),
        $.command_block,
        $.on_command_item,
      ),
    ),
    command: $ => seq(optional($.tool), $.command_text),
    tool: $ => token(prec(1, /@[A-Za-z_][A-Za-z0-9_-]*/)),
    command_text: $ => token(prec(0, /[^ \t@#}\n][^\n]*/)),

    domain_declaration: $ => seq('domain', field('id', $.identifier)),

    postgres: $ => seq(
      'postgres',
      field('id', $.identifier),
      '{',
      repeat($._postgres_item),
      '}',
    ),
    _postgres_item: $ => choice(
      $.database_declaration,
      $.version_declaration,
      $.on_postgres_item,
    ),
    database_declaration: $ => seq('database', field('id', $.identifier)),

    redis: $ => seq(
      'redis',
      field('id', $.identifier),
      optional(seq('{', repeat($._redis_item), '}')),
    ),
    _redis_item: $ => choice($.version_declaration, $.on_redis_item),

    s3: $ => seq(
      's3',
      field('bucket', $.identifier),
      optional(seq('{', repeat($._s3_item), '}')),
    ),
    _s3_item: $ => choice($.policy_declaration, $.cdn_declaration, $.on_s3_item),
    policy_declaration: $ => seq(
      'policy',
      field('policy', choice('private', 'public-read')),
    ),
    cdn_declaration: $ => seq('cdn', field('id', $.identifier)),

    version_declaration: $ => seq('version', $._version),
    _version: $ => choice($.integer, $.version_string),
    version_string: $ => token(/[0-9]+\.[0-9]+(?:\.[0-9]+)?/),

    on_statement: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._declaration),
        seq('{', repeat($._declaration), '}'),
      ),
      optional($._else_declaration),
    ),
    _else_declaration: $ => seq(
      'else',
      choice(
        seq(':', $._declaration),
        seq('{', repeat($._declaration), '}'),
        $.on_statement,
      ),
    ),
    on_image_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._image_item),
        seq('{', repeat($._image_item), '}'),
      ),
      optional($._else_image_item),
    ),
    _else_image_item: $ => seq(
      'else',
      choice(
        seq(':', $._image_item),
        seq('{', repeat($._image_item), '}'),
        $.on_image_item,
      ),
    ),
    on_service_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._service_item),
        seq('{', repeat($._service_item), '}'),
      ),
      optional($._else_service_item),
    ),
    _else_service_item: $ => seq(
      'else',
      choice(
        seq(':', $._service_item),
        seq('{', repeat($._service_item), '}'),
        $.on_service_item,
      ),
    ),
    on_postgres_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._postgres_item),
        seq('{', repeat($._postgres_item), '}'),
      ),
      optional($._else_postgres_item),
    ),
    _else_postgres_item: $ => seq(
      'else',
      choice(
        seq(':', $._postgres_item),
        seq('{', repeat($._postgres_item), '}'),
        $.on_postgres_item,
      ),
    ),
    on_redis_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._redis_item),
        seq('{', repeat($._redis_item), '}'),
      ),
      optional($._else_redis_item),
    ),
    _else_redis_item: $ => seq(
      'else',
      choice(
        seq(':', $._redis_item),
        seq('{', repeat($._redis_item), '}'),
        $.on_redis_item,
      ),
    ),
    on_s3_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._s3_item),
        seq('{', repeat($._s3_item), '}'),
      ),
      optional($._else_s3_item),
    ),
    _else_s3_item: $ => seq(
      'else',
      choice(
        seq(':', $._s3_item),
        seq('{', repeat($._s3_item), '}'),
        $.on_s3_item,
      ),
    ),
    on_env_item: $ => seq(
      'on',
      field('condition', $._expression),
      choice(
        seq(':', $._env_item),
        seq('{', repeat($._env_item), '}'),
      ),
      optional($._else_env_item),
    ),
    _else_env_item: $ => seq(
      'else',
      choice(
        seq(':', $._env_item),
        seq('{', repeat($._env_item), '}'),
        $.on_env_item,
      ),
    ),

    _expression: $ => choice(
      $.string,
      $.integer,
      $.boolean,
      $.port_reference,
      $.env_reference,
      $.call,
      $.property,
      $.identifier,
      $.unary_expression,
      $.binary_expression,
      $.fallback_expression,
    ),
    // A double-quoted string: plain text plus `"${<expr>}"` interpolation sites.
    string: $ => seq(
      '"',
      repeat(choice($.string_escape, $.interpolation, $.string_text, $.string_dollar)),
      '"',
    ),
    string_escape: $ => token.immediate(/\\./),
    string_text: $ => token.immediate(/[^"\\$\n]+/),
    string_dollar: $ => token.immediate(/\$/),
    interpolation: $ => seq('${', $._expression, '}'),
    integer: $ => token(/[0-9]+/),
    boolean: $ => choice('true', 'false'),
    port_reference: $ => token('$port'),
    env_reference: $ => token(/\$env\.[A-Za-z_][A-Za-z0-9_]*/),
    property: $ => prec(5, seq(
      field('base', $.identifier),
      '.',
      field('property', $.identifier),
    )),
    call: $ => prec(6, seq(
      field('function', $._call_name),
      '(',
      optional(seq($._call_argument, repeat(seq(',', $._call_argument)))),
      ')',
    )),
    _call_argument: $ => choice($.identifier, $.string, $.integer, $.call),
    _call_name: $ => choice(
      'var',
      'secret',
      'random_string',
      'random_hex',
      'random_base64',
      'random_uuid',
    ),
    unary_expression: $ => prec(6, seq('!', $._expression)),
    binary_expression: $ => choice(
      prec.left(1, seq($._expression, '|', $._expression)),
      prec.left(2, seq($._expression, '&', $._expression)),
      prec.left(3, seq($._expression, '+', $._expression)),
    ),
    fallback_expression: $ => prec.left(0, seq($._expression, '??', $._expression)),

    identifier: $ => /[A-Za-z_][A-Za-z0-9_-]*/,
  },
});

function commaSep(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}
