/**
 * Tree-sitter grammar for Slipfork's `Forkfile` manifest (`docs/design/forkfile.md`).
 *
 * `Forkfile` is line-oriented: a newline terminates a statement and a raw
 * `run`/hook command line. `extras` therefore holds spaces, tabs, and carriage
 * returns but never `\n`. Braces, brackets, and parentheses accept several
 * statements on one line, separated by newlines or semicolons.
 *
 * This file mirrors `crates/slipfork-forkfile/src/` exactly; anything the
 * shipped parser rejects must not parse here either.
 */

function items($, rule) {
  return seq(
    repeat($._terminator),
    optional(seq(
      rule,
      repeat(seq(repeat1($._terminator), rule)),
      repeat($._terminator),
    )),
  );
}

module.exports = grammar({
  name: 'forkfile',

  extras: $ => [/[ \t\r]/, $.comment],

  word: $ => $.identifier,

  conflicts: $ => [[$._statement, $._service_item]],

  rules: {
    source_file: $ => items($, $._statement),

    comment: $ => token(/#[^\n]*/),
    _terminator: $ => token(choice('\n', ';')),

    identifier: $ => /[A-Za-z_][A-Za-z0-9_-]*/,
    integer: $ => token(/\d+/),
    string: $ => token(seq('"', repeat(choice(/[^"\\\n]/, /\\./)), '"')),
    raw_string: $ => token(seq("'", repeat(/[^'\n]/), "'")),
    boolean: $ => token(prec(1, choice('true', 'false'))),
    reference: $ => token(/\$[A-Za-z_][A-Za-z0-9_.-]*/),
    path: $ => token(choice(
      seq('./', repeat(/[^\s{}\[\]()"';]/)),
      seq('..', repeat(/[^\s{}\[\]()"';]/)),
      seq('/', repeat(/[^\s{}\[\]()"';]/)),
      seq('.', /[A-Za-z_]/, repeat(/[^\s{}\[\]()"';]/)),
    )),
    route_suffix: $ => token(/[A-Za-z0-9_.-]+\.\*/),
    image_ref: $ => token(/[A-Za-z0-9][A-Za-z0-9_.\-\/:@]*/),
    flag: $ => token(/-{1,2}[A-Za-z0-9][A-Za-z0-9_-]*/),
    _command_punctuation: $ => token(choice(
      ':', '=', ',', '.', '/', '-', '*', '?', '+', '&', '|', '!', '@', '%', '^', '~',
    )),

    _expression: $ => choice(
      $.string,
      $.raw_string,
      $.integer,
      $.boolean,
      $.reference,
      $.reference_path,
      $.function_call,
      $.list,
      $.binary_expression,
      $.identifier,
    ),
    list: $ => seq('[', optional(seq($._expression, repeat(seq(',', $._expression)))), ']'),
    reference_path: $ => prec(1, seq($.identifier, repeat1(seq('.', $.identifier)))),
    function_call: $ => prec(1, seq($.identifier, '(', optional($.arguments), ')')),
    arguments: $ => seq($._expression, repeat(seq(',', $._expression))),
    binary_expression: $ => prec.left(2, seq(
      $._expression,
      field('operator', choice('+', '??')),
      $._expression,
    )),

    _statement: $ => choice(
      $.project_statement,
      $.features_statement,
      $.postgres_statement,
      $.redis_statement,
      $.s3_statement,
      $.domain_statement,
      $.env_group,
      $.service_statement,
      $.task_statement,
      $.on_statement,
    ),

    project_statement: $ => seq('project', field('name', $.identifier)),

    features_statement: $ => seq('features', $.features_block),
    features_block: $ => seq('{', items($, $.feature_declaration), '}'),
    feature_declaration: $ => seq(
      field('name', $.identifier),
      optional(seq('=', $.feature_list)),
    ),
    feature_list: $ => seq('[', optional(seq($.identifier, repeat(seq(',', $.identifier)))), ']'),

    postgres_statement: $ => seq('postgres', field('name', $.identifier), optional($.resource_block)),
    redis_statement: $ => seq('redis', field('name', $.identifier), optional($.resource_block)),
    s3_statement: $ => seq('s3', field('name', $.identifier)),
    domain_statement: $ => seq('domain', field('name', $.identifier)),

    // `resources.rs`: a resource block holds only `version`, `database`, and `on`.
    resource_block: $ => seq('{', items($, $._resource_item), '}'),
    _resource_item: $ => choice(
      $.version_statement,
      $.database_statement,
      $.on_statement,
    ),
    version_statement: $ => seq('version', field('value', choice($.integer, $.string))),
    database_statement: $ => seq('database', field('name', $.identifier), optional($.database_block)),
    database_block: $ => seq('{', items($, choice($.assignment, $.on_statement)), '}'),
    assignment: $ => seq(field('key', $.identifier), '=', field('value', $._expression)),

    env_group: $ => seq('env', field('name', $.identifier), $.env_block),
    env_section: $ => seq('env', $.env_block),
    build_env_block: $ => seq('build_env', $.env_block),
    env_block: $ => seq('{', items($, $.env_assignment), '}'),
    env_assignment: $ => seq(field('key', $.identifier), '=', field('value', $._expression)),

    service_statement: $ => seq(
      'service',
      field('name', $.identifier),
      field('source', choice($.path, seq('from', $.image_ref))),
      $.service_block,
    ),
    service_block: $ => seq('{', items($, $._service_item), '}'),
    _service_item: $ => choice(
      $.port_statement,
      $.route_statement,
      $.internal_ports_statement,
      $.volume_statement,
      $.dockerfile_statement,
      $.stage_statement,
      $.size_statement,
      $.use_env_statement,
      $.run_statement,
      $.hook_statement,
      $.env_section,
      $.build_env_block,
      $.env_key_assignment,
      $.on_statement,
    ),
    port_statement: $ => seq('port', field('value', $.integer)),
    route_statement: $ => seq('route', field('value', choice($.route_suffix, $.path))),
    internal_ports_statement: $ => seq('internal_ports', $.integer_list),
    integer_list: $ => seq('[', optional(seq($.integer, repeat(seq(',', $.integer)))), ']'),
    volume_statement: $ => seq('volume', field('value', choice($.path, $.string))),
    dockerfile_statement: $ => seq('dockerfile', field('value', choice($.path, $.identifier))),
    stage_statement: $ => seq('stage', field('value', $.identifier)),
    size_statement: $ => seq('size', field('value', $.string)),
    use_env_statement: $ => seq('use', 'env', field('group', $.identifier)),
    env_key_assignment: $ => seq('env', '.', field('key', $.identifier), '=', field('value', $._expression)),

    run_statement: $ => seq('run', $.command_line),
    hook_name: $ => choice('setup', 'pre-deploy', 'post-deploy', 'failed-deploy'),
    hook_statement: $ => seq(
      $.hook_name,
      choice(seq(':', $.command_line), seq('{', items($, $.command_line), '}')),
    ),
    command_line: $ => repeat1($._command_word),
    _command_word: $ => choice(
      $.identifier,
      $.string,
      $.raw_string,
      $.integer,
      $.reference,
      $.path,
      $.flag,
      $._command_punctuation,
    ),

    task_statement: $ => seq(
      'task',
      field('name', $.identifier),
      'in',
      field('service', $.identifier),
      choice(seq(':', $.command_line), seq('{', items($, $.command_line), '}')),
    ),

    on_statement: $ => seq(
      'on',
      field('condition', $._condition),
      choice(seq(':', $._body_statement), $.body_block),
    ),
    body_block: $ => seq('{', items($, $._body_statement), '}'),
    _body_statement: $ => choice($._statement, $._service_item),
    _condition: $ => choice(
      $.condition_identifier,
      $.negated_condition,
      $.and_condition,
      $.or_condition,
      $.parenthesized_condition,
    ),
    condition_identifier: $ => $.identifier,
    negated_condition: $ => seq('!', $._condition),
    and_condition: $ => prec.left(2, seq($._condition, '&', $._condition)),
    or_condition: $ => prec.left(1, seq($._condition, '|', $._condition)),
    parenthesized_condition: $ => seq('(', $._condition, ')'),
  },
});
