/**
 * Pure Dependency-Injected core of `@slipfork/opencode` (`ADR-0004`, `M2.18`).
 *
 * Zero `node:fs`, `node:child_process`, or `process.env` calls here: the project state is loaded
 * once at session start by an injected `ManifestProbe`, and every CLI effect flows through the
 * injected `SlipforkGateway`, so the whole policy is unit-testable with fakes.
 */

import { join } from "node:path";

import {
  type ForkedWorld,
  type SlipforkWorldRow,
  WORKTREES_DIRECTORY,
} from "./cli.ts";

/** The single project manifest name (`snow.yml`, `ADR-0001`). */
const MANIFEST_FILE = "snow.yml";

/**
 * Explicit project state built once at session start (`M2.18`).
 *
 * `snowFile` is `null` when the repository has no manifest; `configError` carries an actionable
 * message when the manifest could not be inspected. The worktree gateway never probes the
 * filesystem itself, so a missing or unreadable manifest is a visible state, not a silent skip.
 */
export interface SlipforkProjectState {
  readonly repoRoot: string;
  readonly snowFile: string | null;
  readonly configError?: string;
}

/** Injected host-filesystem port used only by the explicit project-state loader. */
export interface ManifestProbe {
  /** `"present"` for a readable regular file, `"missing"` when absent, else an actionable error. */
  inspect(path: string): "present" | "missing" | { readonly error: string };
}

/** One world bound to one `.worktrees/<fork>` checkout. */
export interface SlipforkWorldBinding {
  /** The `--world` fork name (`<project>`-scoped world ids are the daemon's business). */
  readonly forkId: string;
  /** The CLI-owned checkout directory (`<repoRoot>/.worktrees/<fork>`). */
  readonly cwd: string;
  /** The world's single `WorldOrigin` URL, exactly as `slipfork fork --json` reported it. */
  readonly url: string;
}

/** A structured project-level result the session surfaces instead of skipping silently. */
export type ProjectNotice =
  | { readonly kind: "no-manifest"; readonly message: string }
  | { readonly kind: "config-error"; readonly message: string };

/** The structured outcome of routing one `shell` event. */
export type ShellRouting =
  | {
      readonly kind: "routed";
      readonly forkId: string;
      readonly cwd: string;
      readonly url: string | null;
    }
  | { readonly kind: "untouched"; readonly reason: "outside-project" | "unmanaged-checkout" }
  | ProjectNotice;

/** The `slipfork` CLI port the plugin drives (`ADR-0004`: the plugin only routes). */
export interface SlipforkGateway {
  forkWorld(input: { fork: string; base?: string; signal: AbortSignal }): Promise<ForkedWorld>;
  dropWorld(input: { fork: string; signal: AbortSignal }): Promise<void>;
  /** Every world from one `slipfork status --json` read (`worlds[].worktree`), failing closed. */
  listWorlds(input: { signal: AbortSignal }): Promise<readonly SlipforkWorldRow[]>;
  /** Build the host-shell command line that enters `fork` at `cwd`. Never resolves env vars. */
  wrapShellCommand(input: {
    fork: string;
    cwd: string;
    shell: string;
    command: string;
  }): { shell: string; command: string; env: Record<string, string> };
}

export interface ShellCreateBeforeEvent {
  command: string;
  cwd: string;
  timeout: number;
  shell: string;
  env: Record<string, string | undefined>;
}

export interface SessionContextEvent {
  readonly sessionID: string;
  system: { type: "text"; text: string }[];
}

/** Load the explicit project state once, through the injected probe (`M2.18`). */
export function loadSlipforkProjectState(input: {
  repoRoot: string;
  probe: ManifestProbe;
}): SlipforkProjectState {
  const snowPath = join(input.repoRoot, MANIFEST_FILE);
  const inspected = input.probe.inspect(snowPath);
  if (typeof inspected === "object") {
    return { repoRoot: input.repoRoot, snowFile: null, configError: inspected.error };
  }
  return {
    repoRoot: input.repoRoot,
    snowFile: inspected === "present" ? snowPath : null,
  };
}

/** `true` when `cwd` is `directory` or lives under it. */
function isInside(directory: string, cwd: string): boolean {
  return cwd === directory || cwd.startsWith(`${directory}/`);
}

/** Strip trailing slashes so configured roots and event `cwd` values compare literally. */
function normalizeDirectory(directory: string): string {
  return directory.replace(/\/+$/, "") || "/";
}

function basename(directory: string): string {
  const parts = normalizeDirectory(directory).split("/");
  return parts[parts.length - 1] ?? "";
}

/** The fork whose CLI-owned checkout contains `cwd`, if any (`<repoRoot>/.worktrees/<fork>`). */
function forkFromCheckout(repoRoot: string, cwd: string): string | undefined {
  const worktrees = join(repoRoot, WORKTREES_DIRECTORY);
  if (!isInside(worktrees, cwd)) return undefined;
  const rest = cwd === worktrees ? "" : cwd.slice(worktrees.length + 1);
  const fork = rest.split("/")[0];
  return fork.length > 0 ? fork : undefined;
}

/** One local worktree strategy registered through `ctx.worktree.transform` (OpenCode V2). */
export interface WorktreeStrategy {
  readonly id: string;
  create(
    input: { sourceDirectory: string; directory: string; branch?: string },
    context: { signal: AbortSignal },
  ): Promise<{ directory: string }>;
  remove(
    input: { directory: string; force: boolean },
    context: { signal: AbortSignal },
  ): Promise<void>;
  list(
    sourceDirectory: string,
    context: { signal: AbortSignal },
  ): Promise<readonly { directory: string; type: "root" | "worktree" }[]>;
}

export class SlipforkWorktreeCoordinator {
  private readonly bindings = new Map<string, SlipforkWorldBinding>();
  private readonly dropped = new Set<string>();

  constructor(
    private readonly project: SlipforkProjectState,
    private readonly slipfork: SlipforkGateway,
  ) {}

  /** The structured no-manifest / config-error notice, if the project cannot provide worlds. */
  projectNotice(): ProjectNotice | undefined {
    if (this.project.configError !== undefined) {
      return {
        kind: "config-error",
        message:
          `Slipfork cannot read ${this.project.repoRoot}: ${this.project.configError}. ` +
          `Slipfork worlds and shell routing are disabled.`,
      };
    }
    if (this.project.snowFile === null) {
      return {
        kind: "no-manifest",
        message:
          `no ${MANIFEST_FILE} found in ${this.project.repoRoot}; ` +
          `Slipfork worlds and shell routing are disabled. ` +
          `Add ${MANIFEST_FILE} at the repository root, or remove ` +
          `@slipfork/opencode from .opencode/opencode.jsonc.`,
      };
    }
    return undefined;
  }

  /** The binding whose checkout contains `cwd` (longest checkout wins). */
  bindingForDirectory(cwd: string): SlipforkWorldBinding | undefined {
    const target = normalizeDirectory(cwd);
    let best: SlipforkWorldBinding | undefined;
    for (const binding of this.bindings.values()) {
      const contains = isInside(binding.cwd, target);
      const longest = best === undefined || binding.cwd.length > best.cwd.length;
      if (contains && longest) best = binding;
    }
    return best;
  }

  makeWorktreeStrategy(): WorktreeStrategy {
    return {
      id: "slipfork.cow-worktree",
      create: async (
        input: { sourceDirectory: string; directory: string; branch?: string },
        context: { signal: AbortSignal },
      ): Promise<{ directory: string }> => {
        // `slipfork fork <fork>` owns the whole checkout atomically: the git worktree, its branch,
        // and the CoW world. The CLI names it `<repoRoot>/.worktrees/<fork>`, so the suggested
        // directory's basename is the fork name and the returned directory is the CLI's real path.
        const fork = basename(input.directory);
        const forked = await this.slipfork.forkWorld({
          fork,
          base: input.branch,
          signal: context.signal,
        });
        const cwd = join(this.project.repoRoot, WORKTREES_DIRECTORY, fork);
        this.dropped.delete(fork);
        this.bindings.set(cwd, { forkId: fork, cwd, url: forked.origin });
        return { directory: cwd };
      },
      remove: async (
        input: { directory: string; force: boolean },
        context: { signal: AbortSignal },
      ): Promise<void> => {
        // `force` belongs to OpenCode's Git strategy; `slipfork drop` always tears the fork down.
        const binding = this.bindingForDirectory(input.directory);
        const fork =
          binding?.forkId ??
          forkFromCheckout(this.project.repoRoot, input.directory) ??
          basename(input.directory);
        await this.dropFork(fork, context.signal);
        if (binding !== undefined) this.bindings.delete(binding.cwd);
      },
      list: async (
        sourceDirectory: string,
        context: { signal: AbortSignal },
      ): Promise<readonly { directory: string; type: "root" | "worktree" }[]> => {
        const rows = await this.slipfork.listWorlds({ signal: context.signal });
        const worktrees = join(this.project.repoRoot, WORKTREES_DIRECTORY);
        const owned = rows
          .map((row) => normalizeDirectory(row.worktree))
          .filter((directory) => isInside(worktrees, directory))
          .map((directory) => ({ directory, type: "worktree" as const }));
        return [
          { directory: normalizeDirectory(sourceDirectory), type: "root" },
          ...owned,
        ];
      },
    };
  }

  /**
   * Route one shell command into its world (`ADR-0004` §2).
   *
   * Only a checkout the plugin owns is rewritten; the canonical checkout and unrelated
   * directories stay untouched. The command is passed through verbatim to `slipfork exec`, which
   * is the sole owner of `snow.yml` environment resolution.
   */
  handleShellBefore(event: ShellCreateBeforeEvent): ShellRouting {
    const cwd = normalizeDirectory(event.cwd);
    if (!isInside(normalizeDirectory(this.project.repoRoot), cwd)) {
      return { kind: "untouched", reason: "outside-project" };
    }
    const notice = this.projectNotice();
    if (notice !== undefined) return notice;

    const binding = this.bindingForDirectory(cwd);
    const forkId = binding?.forkId ?? forkFromCheckout(this.project.repoRoot, cwd);
    if (forkId === undefined) {
      return { kind: "untouched", reason: "unmanaged-checkout" };
    }

    const wrapped = this.slipfork.wrapShellCommand({
      fork: forkId,
      cwd,
      shell: event.shell,
      command: event.command,
    });
    event.shell = wrapped.shell;
    event.command = wrapped.command;
    for (const [key, value] of Object.entries(wrapped.env)) {
      event.env[key] = value;
    }
    return { kind: "routed", forkId, cwd, url: binding?.url ?? null };
  }

  /** Build the 2-line `<context key="slipfork-world">` note for one session directory. */
  buildContextHint(sessionDirectory: string): string | undefined {
    const binding = this.bindingForDirectory(sessionDirectory);
    if (binding === undefined) return undefined;
    return (
      `<context key="slipfork-world">\n` +
      `Active Slipfork CoW world '${binding.forkId}' for ${binding.cwd}, ` +
      `single origin ${binding.url}. ` +
      `\`shell\` runs inside the world with snow.yml envs injected by \`slipfork exec\`; ` +
      `never connect to the host PostgreSQL :5432 or app :8000 directly.\n` +
      `</context>`
    );
  }

  /** Build the `<context key="slipfork-project">` note when the project cannot provide worlds. */
  buildProjectNoticeHint(): string | undefined {
    const notice = this.projectNotice();
    if (notice === undefined) return undefined;
    return `<context key="slipfork-project">\n${notice.message}\n</context>`;
  }

  /**
   * Release every bound world for an unloading plugin (`M2.18`).
   *
   * Teardown is idempotent and never throws: a failed release is returned as an actionable
   * warning so the caller records it instead of crashing a cleanup callback.
   */
  async releaseAllWorlds(signal: AbortSignal): Promise<readonly string[]> {
    const warnings: string[] = [];
    const forks = new Set([...this.bindings.values()].map((binding) => binding.forkId));
    for (const fork of forks) {
      try {
        await this.dropFork(fork, signal);
      } catch (err) {
        warnings.push(`cannot drop world '${fork}': ${(err as Error).message}`);
      }
    }
    this.bindings.clear();
    return warnings;
  }

  private async dropFork(fork: string, signal: AbortSignal): Promise<void> {
    if (this.dropped.has(fork)) return;
    await this.slipfork.dropWorld({ fork, signal });
    this.dropped.add(fork);
  }
}
