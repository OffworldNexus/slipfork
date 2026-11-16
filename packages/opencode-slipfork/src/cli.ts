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
    list: "list",
  },
  flag: {
    json: "--json",
    base: "--base",
    world: "--world",
    cwd: "--cwd",
  },
  field: {
    world: "world",
    origin: "origin",
    worktree: "worktree",
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

/** One row of `slipfork --json list`. */
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

/** `slipfork --json list`: every world with its state, worktree, and one origin URL. */
export function buildListArgs(): string[] {
  return [SLIPFORK.flag.json, SLIPFORK.command.list];
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

/** Decode and validate one `slipfork --json list` payload. */
export function parseWorldList(stdout: string): SlipforkWorldRow[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch (err) {
    throw new Error(`slipfork list did not print JSON: ${(err as Error).message}`);
  }
  if (!Array.isArray(parsed)) {
    throw new Error("slipfork list did not print a JSON array of worlds");
  }
  return parsed.map((row, index) => {
    if (typeof row !== "object" || row === null) {
      throw new Error(`slipfork list row ${index} is not an object`);
    }
    const record = row as Record<string, unknown>;
    const world = requireString(record, SLIPFORK.field.world, `slipfork list row ${index}`);
    const origin = requireString(record, SLIPFORK.field.origin, `slipfork list row ${index}`);
    const worktree = requireString(record, SLIPFORK.field.worktree, `slipfork list row ${index}`);
    requireHttpOrigin(origin, `slipfork list row ${index}`);
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

function requireString(record: Record<string, unknown>, field: string, what: string): string {
  const value = record[field];
  if (typeof value !== "string" || value.length === 0) {
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
