/**
 * The one declaration of the `slipfork` CLI surface the plugin drives.
 *
 * Argument lists, JSON decoders, and the `slipfork` binary lookup live here; the coordinator
 * never spells a subcommand or a JSON field, and `src/index.ts` injects the environment.
 */
import { execFile } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { delimiter, join } from "node:path";
import { promisify } from "node:util";

import type { ForkedWorld, SlipforkGateway, WorldRow } from "./coordinator.ts";

const run = promisify(execFile);

/** The CLI subcommands and flags this plugin is allowed to spell. */
const SURFACE = {
  fork: "fork",
  drop: "drop",
  status: "status",
  json: "--json",
  force: "--force",
} as const;

/** One typed CLI failure; `message` already carries the CLI's own `next:` remedy. */
export class SlipforkCliError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SlipforkCliError";
  }
}

/** `slipfork fork <fork> --json`: the fork name is positional. */
export function buildForkArgs(fork: string): string[] {
  return [SURFACE.fork, fork, SURFACE.json];
}

/** `slipfork drop <fork> [--force] --json`. */
export function buildDropArgs(fork: string, force: boolean): string[] {
  const args = [SURFACE.drop, fork, SURFACE.json];
  if (force) args.push(SURFACE.force);
  return args;
}

/** `slipfork status --json`: the fleet document every fork row lives in. */
export function buildStatusArgs(): string[] {
  return [SURFACE.status, SURFACE.json];
}

/**
 * The binary named by `options.binary`, else the first executable `slipfork` on `PATH`.
 *
 * `SYSTEMROOT` is what Windows resolves DLLs with; its presence also selects the Windows path
 * delimiter and `.exe` suffix. Nothing else from the environment is consulted.
 */
export function resolveBinary(
  explicit: string | undefined,
  env: { readonly PATH?: string | undefined; readonly SYSTEMROOT?: string | undefined },
): string | undefined {
  if (explicit !== undefined && explicit.length > 0) return explicit;
  const windows = env.SYSTEMROOT !== undefined && env.SYSTEMROOT.length > 0;
  const names = windows ? ["slipfork.exe", "slipfork"] : ["slipfork"];
  const search = (env.PATH ?? "").split(windows ? ";" : delimiter);
  for (const directory of search) {
    if (directory.length === 0) continue;
    for (const name of names) {
      const candidate = join(directory, name);
      if (isExecutable(candidate)) return candidate;
    }
  }
  return undefined;
}

function isExecutable(path: string): boolean {
  try {
    if (!statSync(path).isFile()) return false;
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** `slipfork --json` failures carry `{"error":{"code","message","next"}}`. */
export function decodeCliFailure(text: string): string | undefined {
  const payload = parseObject(text);
  if (payload === undefined) return undefined;
  const error = payload.error;
  if (typeof error !== "object" || error === null) return undefined;
  const record = error as Record<string, unknown>;
  const message = presentString(record.message);
  const next = presentString(record.next);
  const lines: string[] = [];
  if (message !== undefined) lines.push(message);
  if (next !== undefined) lines.push(`next: ${next}`);
  if (lines.length === 0) return undefined;
  return lines.join("\n");
}

/** Decode one `slipfork fork --json` success document. */
export function parseForkedWorld(stdout: string, expectedFork: string): ForkedWorld {
  const payload = parseObjectOrThrow(stdout, "slipfork fork");
  const fork = requireString(payload, "fork", "slipfork fork");
  if (fork !== expectedFork) {
    throw new SlipforkCliError(
      `slipfork fork reported '${fork}' for '${expectedFork}'; next: retry with the fork name you passed`,
    );
  }
  const world = requireString(payload, "world", "slipfork fork");
  if (world !== fork && !world.startsWith(`${fork}--`)) {
    throw new SlipforkCliError(
      `slipfork fork reported world '${world}' for fork '${fork}'; expected '${fork}--<world>'`,
    );
  }
  const project = requireString(payload, "project", "slipfork fork");
  const url = requireHttpUrl(requireString(payload, "url", "slipfork fork"), "slipfork fork");
  return { fork, world, project, url };
}

/** Decode `slipfork status --json`, keeping top-level worlds and their nested forks. */
export function parseStatusWorlds(stdout: string): WorldRow[] {
  const payload = parseObjectOrThrow(stdout, "slipfork status");
  const worlds = payload.worlds;
  if (!Array.isArray(worlds)) {
    throw new SlipforkCliError("slipfork status did not print a 'worlds' array");
  }
  return worlds.map((row, index) => readWorldRow(row, `world ${index}`));
}

/** Every top-level world and nested fork, flattened; only forks carry `fork`. */
export function flattenForkRows(rows: readonly WorldRow[]): WorldRow[] {
  const flat: WorldRow[] = [];
  for (const row of rows) {
    flat.push(row);
    flat.push(...flattenForkRows(row.forks));
  }
  return flat;
}

function readWorldRow(value: unknown, what: string): WorldRow {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new SlipforkCliError(`slipfork status ${what} is not an object`);
  }
  const record = value as Record<string, unknown>;
  const fork = presentString(record.fork);
  const forks = Array.isArray(record.forks)
    ? record.forks.map((child, index) => readWorldRow(child, `${what} fork ${index}`))
    : [];
  return {
    world: requireString(record, "world", `slipfork status ${what}`),
    project: requireString(record, "project", `slipfork status ${what}`),
    path: requireString(record, "path", `slipfork status ${what}`),
    url: requireHttpUrl(
      requireString(record, "url", `slipfork status ${what}`),
      `slipfork status ${what}`,
    ),
    ...(fork === undefined ? {} : { fork }),
    forks,
  };
}

function parseObject(text: string): Record<string, unknown> | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text.trim());
  } catch {
    return undefined;
  }
  return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)
    ? (parsed as Record<string, unknown>)
    : undefined;
}

function parseObjectOrThrow(text: string, what: string): Record<string, unknown> {
  const payload = parseObject(text);
  if (payload === undefined) {
    throw new SlipforkCliError(`${what} did not print a JSON object`);
  }
  const failure = decodeCliFailure(text);
  if (failure !== undefined) throw new SlipforkCliError(failure);
  return payload;
}

function presentString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function requireString(record: Record<string, unknown>, field: string, what: string): string {
  const value = presentString(record[field]);
  if (value === undefined) {
    throw new SlipforkCliError(`${what} JSON is missing the '${field}' string field`);
  }
  return value;
}

function requireHttpUrl(value: string, what: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new SlipforkCliError(`${what} reported an unparseable URL '${value}'`);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new SlipforkCliError(`${what} reported a non-HTTP URL '${value}'`);
  }
  return value;
}

export interface CliGatewayOptions {
  /** The executable `src/index.ts` resolved from `options.binary` or `PATH`. */
  readonly binary: string;
  /** The allowlisted child environment the composition root handed the plugin. */
  readonly env: Readonly<Record<string, string | undefined>>;
}

/** Thin `slipfork` adapter: forward argv, decode JSON, surface the CLI's own remedy. */
export class CliSlipforkGateway implements SlipforkGateway {
  constructor(private readonly options: CliGatewayOptions) {}

  async forkWorld(input: { fork: string; cwd: string }): Promise<ForkedWorld> {
    const stdout = await this.invoke(buildForkArgs(input.fork), input.cwd, `fork '${input.fork}'`);
    return parseForkedWorld(stdout, input.fork);
  }

  async dropWorld(input: { fork: string; cwd: string; force: boolean }): Promise<void> {
    await this.invoke(
      buildDropArgs(input.fork, input.force),
      input.cwd,
      `drop '${input.fork}'`,
    );
  }

  async listWorlds(input: { cwd: string }): Promise<readonly WorldRow[]> {
    return parseStatusWorlds(await this.invoke(buildStatusArgs(), input.cwd, "status"));
  }

  private async invoke(args: readonly string[], cwd: string, what: string): Promise<string> {
    try {
      const { stdout } = await run(this.options.binary, [...args], {
        cwd,
        env: this.options.env,
        encoding: "utf8",
        maxBuffer: 8 * 1024 * 1024,
      });
      return String(stdout);
    } catch (error) {
      const failure = error as { stdout?: unknown; stderr?: unknown };
      const stdout = typeof failure.stdout === "string" ? failure.stdout : "";
      const stderr = typeof failure.stderr === "string" ? failure.stderr : "";
      const decoded = decodeCliFailure(stdout) ?? decodeCliFailure(stderr);
      if (decoded !== undefined) {
        throw new SlipforkCliError(`slipfork ${what} failed: ${decoded}`);
      }
      throw new SlipforkCliError(
        `slipfork ${what} failed\nnext: run \`slipfork doctor\``,
      );
    }
  }
}
