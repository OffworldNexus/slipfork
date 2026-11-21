/**
 * Pure policy for `@slipfork/opencode`: every CLI effect flows through the injected
 * {@link SlipforkGateway}, and every path decision is derived from the injected project root.
 *
 * The coordinator owns one rule the CLI cannot know: a fork is reaped only when this plugin
 * created it, so a session that merely opens an existing checkout never destroys it.
 */
import { basename } from "node:path";

import { flattenForkRows } from "./cli.ts";

/** `specs/03` §1: every fork checkout is `<project>/.forks/<fork>`. */
export const FORKS_DIRECTORY = ".forks";

/** One fork as `slipfork fork --json` reports it. */
export interface ForkedWorld {
  readonly fork: string;
  /** The world id (`<fork>--<world>`) the ingress routes on. */
  readonly world: string;
  /** The CLI-owned checkout directory. */
  readonly project: string;
  readonly url: string;
}

/** One row of `slipfork status --json`, nested under `forks`. */
export interface WorldRow {
  readonly world: string;
  readonly project: string;
  readonly path: string;
  readonly url: string;
  readonly fork?: string;
  readonly forks: WorldRow[];
}

/** The CLI port: fork, drop, and list, each running from an explicit directory. */
export interface SlipforkGateway {
  forkWorld(input: { fork: string; cwd: string }): Promise<ForkedWorld>;
  dropWorld(input: { fork: string; cwd: string; force: boolean }): Promise<void>;
  listWorlds(input: { cwd: string }): Promise<readonly WorldRow[]>;
}

/** One `create.before` shell event: the plugin edits `env` and leaves the command untouched. */
export interface ShellCreateBefore {
  command: string;
  cwd: string;
  timeout: number;
  shell: string;
  env: Record<string, string | undefined>;
}

/** The outcome of inspecting one shell event. */
export type ShellRouting =
  | { readonly kind: "routed"; readonly fork: string; readonly world: string }
  | {
      readonly kind: "untouched";
      readonly reason: "outside-project" | "not-a-fork" | "world-unknown";
    };

/** One worktree entry OpenCode inventories. */
export interface WorktreeEntry {
  readonly directory: string;
  readonly type: "root" | "worktree";
}

/** One fork checkout this plugin opened, with the coordinates the CLI reported. */
export interface Binding {
  readonly directory: string;
  readonly fork: string;
  readonly world: string;
  readonly url: string;
  /** `true` only for a fork this plugin created, never one it merely observed. */
  readonly owned: boolean;
}

/** Normalize trailing slashes so configured roots and event `cwd` values compare literally. */
export function normalizeDirectory(directory: string): string {
  const trimmed = directory.replace(/\/+$/, "");
  return trimmed.length === 0 ? "/" : trimmed;
}

/** `true` when `cwd` is `directory` or lives under it. */
export function isInside(directory: string, cwd: string): boolean {
  const root = normalizeDirectory(directory);
  const target = normalizeDirectory(cwd);
  return target === root || target.startsWith(`${root}/`);
}

/** The `<fork>` in `<project>/.forks/<fork>/…`, if `cwd` is inside a checkout. */
export function forkFromCheckout(projectRoot: string, cwd: string): string | undefined {
  const forks = joinForks(projectRoot);
  if (!isInside(forks, cwd)) return undefined;
  const target = normalizeDirectory(cwd);
  const rest = target === forks ? "" : target.slice(forks.length + 1);
  const fork = rest.split("/")[0];
  return fork !== undefined && fork.length > 0 ? fork : undefined;
}

/** The checkout directory the CLI owns for one fork. */
export function forkDirectory(projectRoot: string, fork: string): string {
  return `${joinForks(projectRoot)}/${fork}`;
}

function joinForks(projectRoot: string): string {
  return `${normalizeDirectory(projectRoot)}/${FORKS_DIRECTORY}`;
}

/** One line naming the world a fork checkout belongs to. */
export function describeWorld(binding: Binding): string {
  return (
    `Slipfork world '${binding.world}' (fork '${binding.fork}') is this checkout's world: ` +
    `${binding.url}. The plugin exports SLIPFORK_WORLD + SLIPFORK_URL so your shell commands and previews can target the fork.`
  );
}

export class SlipforkWorktreeCoordinator {
  private readonly bindings = new Map<string, Binding>();
  private readonly sessions = new Map<string, Set<string>>();
  private readonly dropped = new Set<string>();

  constructor(
    private readonly projectRoot: string,
    private readonly gateway: SlipforkGateway,
  ) {}

  /** `create`: fork from `input.sourceDirectory`, named by the suggested directory. */
  async createFork(input: {
    sourceDirectory: string;
    directory: string;
    branch?: string;
  }): Promise<{ directory: string }> {
    const forked = await this.gateway.forkWorld({
      fork: basename(normalizeDirectory(input.directory)),
      cwd: normalizeDirectory(input.sourceDirectory),
    });
    return { directory: this.adoptFork(forked) };
  }

  /** Record a fork the CLI just created; it becomes reapable by the session that owns it. */
  adoptFork(forked: ForkedWorld): string {
    const directory = normalizeDirectory(forked.project);
    this.dropped.delete(forked.fork);
    this.bindings.set(directory, {
      directory,
      fork: forked.fork,
      world: forked.world,
      url: forked.url,
      owned: true,
    });
    return directory;
  }

  /** `remove`: drop the fork behind `input.directory`, forcing only when asked. */
  async removeFork(input: { directory: string; force: boolean }): Promise<void> {
    const target = normalizeDirectory(input.directory);
    const binding = this.bindingFor(target);
    const fork = binding?.fork ?? forkFromCheckout(this.projectRoot, target) ?? basename(target);
    await this.dropFork(fork, input.force);
    this.bindings.delete(target);
  }

  /** `list`: the project root plus every fork checkout `slipfork status --json` reports. */
  async listForks(sourceDirectory: string): Promise<WorktreeEntry[]> {
    const root = normalizeDirectory(sourceDirectory);
    const forksRoot = joinForks(this.projectRoot);
    const rows = await this.gateway.listWorlds({ cwd: this.projectRoot });
    const entries: WorktreeEntry[] = [{ directory: root, type: "root" }];
    const seen = new Set<string>([root]);
    for (const row of flattenForkRows(rows)) {
      if (row.fork === undefined) continue;
      const directory = normalizeDirectory(row.path);
      if (!isInside(forksRoot, directory) || seen.has(directory)) continue;
      seen.add(directory);
      entries.push({ directory, type: "worktree" });
      this.bindings.set(directory, {
        directory,
        fork: row.fork,
        world: row.world,
        url: row.url,
        owned: this.bindings.get(directory)?.owned ?? false,
      });
    }
    return entries;
  }

  /**
   * `create.before`: when `cwd` is a fork checkout, export its world and URL.
   *
   * The command, shell, and every other environment key pass through untouched; the CLI (not
   * this plugin) resolves the world's `Forkfile` environment.
   */
  async routeShell(event: ShellCreateBefore): Promise<ShellRouting> {
    const cwd = normalizeDirectory(event.cwd);
    if (!isInside(this.projectRoot, cwd)) {
      return { kind: "untouched", reason: "outside-project" };
    }
    const binding = this.bindingFor(cwd);
    const fork = binding?.fork ?? forkFromCheckout(this.projectRoot, cwd);
    if (fork === undefined) return { kind: "untouched", reason: "not-a-fork" };
    if (binding === undefined) return { kind: "untouched", reason: "world-unknown" };
    event.env.SLIPFORK_WORLD = binding.world;
    event.env.SLIPFORK_URL = binding.url;
    return { kind: "routed", fork: binding.fork, world: binding.world };
  }

  /** The one line `session.hook("context")` appends for an agent inside a fork checkout. */
  describeSession(directory: string): string | undefined {
    const binding = this.bindingFor(directory);
    return binding === undefined ? undefined : describeWorld(binding);
  }

  /** Remember which session owns a checkout, but only for a fork this plugin created. */
  bindSession(sessionID: string, directory: string): void {
    const binding = this.bindingFor(directory);
    if (binding === undefined || !binding.owned) return;
    const forks = this.sessions.get(sessionID) ?? new Set<string>();
    forks.add(binding.fork);
    this.sessions.set(sessionID, forks);
  }

  /** Reap exactly the forks `sessionID` created; a failed drop becomes an operator warning. */
  async reapSession(sessionID: string): Promise<string[]> {
    const forks = this.sessions.get(sessionID);
    this.sessions.delete(sessionID);
    const warnings: string[] = [];
    for (const fork of forks ?? []) {
      try {
        await this.dropFork(fork, false);
      } catch (error) {
        warnings.push(`cannot drop fork '${fork}': ${(error as Error).message}`);
      }
    }
    return warnings;
  }

  /** Plugin unload: drop every fork this plugin created, never one it only observed. */
  async releaseAll(): Promise<string[]> {
    const warnings: string[] = [];
    for (const binding of this.bindings.values()) {
      if (!binding.owned) continue;
      try {
        await this.dropFork(binding.fork, false);
      } catch (error) {
        warnings.push(`cannot drop fork '${binding.fork}': ${(error as Error).message}`);
      }
    }
    this.sessions.clear();
    return warnings;
  }

  private bindingFor(directory: string): Binding | undefined {
    const target = normalizeDirectory(directory);
    let best: Binding | undefined;
    for (const binding of this.bindings.values()) {
      if (!isInside(binding.directory, target)) continue;
      if (best === undefined || binding.directory.length > best.directory.length) best = binding;
    }
    return best;
  }

  private async dropFork(fork: string, force: boolean): Promise<void> {
    if (this.dropped.has(fork)) return;
    await this.gateway.dropWorld({ fork, cwd: this.projectRoot, force });
    this.dropped.add(fork);
  }
}
