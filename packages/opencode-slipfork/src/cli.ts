/**
 * The one declaration of the `slipfork` CLI surface the plugin drives (`ADR-0004`, `M2.18`).
 *
 * Nothing else in the plugin spells a subcommand, flag, or JSON field: argument lists and
 * decoders below are the single source, and `tests/cli-contract.critical.test.ts` validates
 * them against `tests/fixtures/cli-contract.json` (recorded from the real binary's `--help`
 * and `--json` output) so a CLI rename fails the suite instead of drifting silently.
 */

/** Subcommands, flags, and JSON fields of the daemon-served `slipfork` CLI. */
export const SLIPFORK = {
  command: {
    fork: "fork",
    drop: "drop",
    exec: "exec",
    status: "status",
  },
  flag: {
    json: "--json",
    base: "--base",
    world: "--world",
    cwd: "--cwd",
  },
  field: {
    ok: "ok",
    ingressPort: "ingress_port",
    worlds: "worlds",
    world: "world",
    origin: "origin",
    worktree: "worktree",
    error: "error",
    next: "next",
  },
} as const;

/** The CLI-owned checkout directory, mirrored from `slipfork-vcs`'s `WORKTREES_DIRECTORY`. */
export const WORKTREES_DIRECTORY = ".worktrees";

/** One fork world as reported by `slipfork --json fork`. */
export interface ForkedWorld {
  /** Fully-qualified daemon world id (`<project>:<fork>`). */
  readonly world: string;
  /** The world's single `WorldOrigin` URL. */
  readonly origin: string;
}

/** One world row of the `slipfork --json status` document. */
export interface SlipforkWorldRow {
  readonly world: string;
  readonly origin: string;
  readonly worktree: string;
}

/** Quote one argv token for the host shell that runs the wrapped command. */
export function shellSingleQuote(text: string): string {
  return `'${text.replaceAll("'", `'\\''`)}'`;
}

/** `slipfork --json fork <fork> [--base <commit-ish>]` (`fork` is positional, `ADR-0008` §4). */
export function buildForkArgs(input: { fork: string; base?: string }): string[] {
  const args = [SLIPFORK.flag.json, SLIPFORK.command.fork, input.fork];
  if (input.base !== undefined) args.push(SLIPFORK.flag.base, input.base);
  return args;
}

/** `slipfork drop <fork>`: the CLI destroys both the world and its `.worktrees/<fork>` checkout. */
export function buildDropArgs(fork: string): string[] {
  return [SLIPFORK.command.drop, fork];
}

/** `slipfork --json status`: the fleet document every world (with worktree and origin) lives in. */
export function buildStatusArgs(): string[] {
  return [SLIPFORK.flag.json, SLIPFORK.command.status];
}

/**
 * `slipfork exec --world <fork> --cwd <cwd> -- <shell> -c <command>`.
 *
 * `slipforkd` alone resolves the world's `snow.yml` environment; the plugin passes the command
 * through untouched.
 */
export function buildExecArgs(input: {
  world: string;
  cwd: string;
  shell: string;
  command: string;
}): string[] {
  return [
    SLIPFORK.command.exec,
    SLIPFORK.flag.world,
    input.world,
    SLIPFORK.flag.cwd,
    input.cwd,
    "--",
    input.shell,
    "-c",
    input.command,
  ];
}

/** Render the host-shell command line that routes one shell event into `world`. */
export function buildExecCommandLine(
  binary: string,
  input: { world: string; cwd: string; shell: string; command: string },
): string {
  return [binary, ...buildExecArgs(input)].map(shellSingleQuote).join(" ");
}

/** Decode and validate one `slipfork --json fork` payload. */
export function parseForkedWorld(stdout: string, fork: string): ForkedWorld {
  const payload = parseJsonObject(stdout, "slipfork fork");
  const world = requireString(payload, SLIPFORK.field.world, "slipfork fork");
  const origin = requireString(payload, SLIPFORK.field.origin, "slipfork fork");
  if (world !== fork && !world.endsWith(`:${fork}`)) {
    throw new Error(
      `slipfork fork reported world '${world}' for fork '${fork}'; ` +
        `expected '${fork}' or '<project>:${fork}'`,
    );
  }
  requireHttpOrigin(origin, "slipfork fork");
  return { world, origin };
}

/**
 * Decode the CLI's `{"ok":false,"code":"error","error":…,"next":…}` failure document into one
 * actionable message (`1 line + next:`), or `undefined` when the text is not that document.
 *
 * `slipfork --json` prints this on stdout for every failed verb, so the adapter never surfaces a
 * silent empty fleet in place of a real error.
 */
export function decodeCliFailure(stdout: string): string | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return undefined;
  const record = parsed as Record<string, unknown>;
  if (record[SLIPFORK.field.ok] !== false) return undefined;
  const error = presentString(record[SLIPFORK.field.error]);
  const next = presentString(record[SLIPFORK.field.next]);
  const lines: string[] = [];
  if (error !== undefined) lines.push(error);
  if (next !== undefined) lines.push(`next: ${next}`);
  if (lines.length === 0) return "the CLI reported failure without a message";
  return lines.join("\n  ");
}

/** Decode and validate one `slipfork --json status` payload, failing closed on `{"ok":false}`. */
export function parseStatusWorlds(stdout: string): SlipforkWorldRow[] {
  const payload = parseJsonObject(stdout, "slipfork status");
  if (payload[SLIPFORK.field.ok] !== true) {
    throw new Error(
      `slipfork status failed: ${decodeCliFailure(stdout) ?? "the JSON document did not report ok"}`,
    );
  }
  const worlds = payload[SLIPFORK.field.worlds];
  if (!Array.isArray(worlds)) {
    throw new Error("slipfork status did not print a 'worlds' array");
  }
  return worlds.map((row, index) => {
    if (typeof row !== "object" || row === null || Array.isArray(row)) {
      throw new Error(`slipfork status world ${index} is not an object`);
    }
    const record = row as Record<string, unknown>;
    const world = requireString(record, SLIPFORK.field.world, `slipfork status world ${index}`);
    const origin = requireString(record, SLIPFORK.field.origin, `slipfork status world ${index}`);
    const worktree = requireString(record, SLIPFORK.field.worktree, `slipfork status world ${index}`);
    requireHttpOrigin(origin, `slipfork status world ${index}`);
    return { world, origin, worktree };
  });
}

function parseJsonObject(stdout: string, what: string): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch (err) {
    throw new Error(`${what} did not print JSON: ${(err as Error).message}`);
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${what} did not print a JSON object`);
  }
  return parsed as Record<string, unknown>;
}

/** The one emptiness rule: an empty or non-string value is a missing field. */
function presentString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function requireString(record: Record<string, unknown>, field: string, what: string): string {
  const value = presentString(record[field]);
  if (value === undefined) {
    throw new Error(`${what} JSON is missing the '${field}' string field`);
  }
  return value;
}

function requireHttpOrigin(origin: string, what: string): void {
  let url: URL;
  try {
    url = new URL(origin);
  } catch {
    throw new Error(`${what} reported an unparseable origin URL '${origin}'`);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(`${what} reported a non-HTTP origin URL '${origin}'`);
  }
}
