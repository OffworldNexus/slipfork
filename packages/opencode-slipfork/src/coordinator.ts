/**
 * Pure Dependency-Injected core of `@slipfork/opencode` (`ADR-0004`).
 *
 * Zero direct `node:child_process` or `process.env` calls here: all external side effects
 * flow through injected `SlipforkGateway` and `GitWorktreeGateway` ports so the adapter is
 * 100% unit-testable with fakes.
 */

export interface ForkedWorldInfo {
  readonly worldId: string;
  readonly directory: string;
}

export interface SlipforkGateway {
  forkWorld(input: { worldName: string; workspaceDir: string; signal: AbortSignal }): Promise<void>;
  dropWorld(input: { worldName: string; signal: AbortSignal }): Promise<void>;
  wrapShellCommand(input: {
    worldName: string;
    cwd: string;
    shell: string;
    command: string;
  }): { shell: string; command: string; env: Record<string, string> };
}

export interface GitWorktreeGateway {
  add(input: {
    sourceDirectory: string;
    directory: string;
    branch?: string;
    signal: AbortSignal;
  }): Promise<void>;
  remove(input: { directory: string; force: boolean; signal: AbortSignal }): Promise<void>;
  list(
    sourceDirectory: string,
    signal: AbortSignal,
  ): Promise<readonly { directory: string; type: "root" | "worktree" }[]>;
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

function slugFromDirectory(directory: string): string {
  const parts = directory.replace(/\/+$/, "").split("/");
  const base = parts[parts.length - 1] ?? "worktree";
  return base.replace(/[^a-zA-Z0-9_-]/g, "-");
}

export class SlipforkWorktreeCoordinator {
  private readonly directoryToWorld = new Map<string, string>();

  constructor(
    private readonly canonicalRoot: string,
    private readonly slipfork: SlipforkGateway,
    private readonly git: GitWorktreeGateway,
  ) {}

  worldForDirectory(cwd: string): string | undefined {
    for (const [dir, world] of this.directoryToWorld.entries()) {
      if (cwd === dir || cwd.startsWith(dir + "/")) {
        return world;
      }
    }
    return undefined;
  }

  makeWorktreeStrategy() {
    return {
      id: "slipfork.cow-worktree",
      create: async (
        input: { sourceDirectory: string; directory: string; branch?: string },
        context: { signal: AbortSignal },
      ): Promise<{ directory: string }> => {
        const worldName = `wt-${slugFromDirectory(input.directory)}`;
        await this.git.add({
          sourceDirectory: input.sourceDirectory,
          directory: input.directory,
          branch: input.branch,
          signal: context.signal,
        });
        try {
          await this.slipfork.forkWorld({
            worldName,
            workspaceDir: input.directory,
            signal: context.signal,
          });
        } catch (err) {
          await this.git.remove({ directory: input.directory, force: true, signal: context.signal });
          throw err;
        }
        this.directoryToWorld.set(input.directory, worldName);
        return { directory: input.directory };
      },
      remove: async (
        input: { directory: string; force: boolean },
        context: { signal: AbortSignal },
      ): Promise<void> => {
        const worldName =
          this.directoryToWorld.get(input.directory) ?? `wt-${slugFromDirectory(input.directory)}`;
        await this.slipfork.dropWorld({ worldName, signal: context.signal });
        await this.git.remove({
          directory: input.directory,
          force: input.force,
          signal: context.signal,
        });
        this.directoryToWorld.delete(input.directory);
      },
      list: async (
        sourceDirectory: string,
        context: { signal: AbortSignal },
      ): Promise<readonly { directory: string; type: "root" | "worktree" }[]> => {
        return this.git.list(sourceDirectory, context.signal);
      },
    };
  }

  handleShellBefore(event: ShellCreateBeforeEvent): void {
    const worldName =
      this.worldForDirectory(event.cwd) ??
      (event.cwd === this.canonicalRoot || event.cwd.startsWith(this.canonicalRoot + "/")
        ? undefined
        : undefined);
    if (!worldName) return;

    const wrapped = this.slipfork.wrapShellCommand({
      worldName,
      cwd: event.cwd,
      shell: event.shell,
      command: event.command,
    });
    event.shell = wrapped.shell;
    event.command = wrapped.command;
    for (const [k, v] of Object.entries(wrapped.env)) {
      event.env[k] = v;
    }
  }

  buildContextHint(sessionDirectory: string): string | undefined {
    const worldName = this.worldForDirectory(sessionDirectory);
    if (!worldName) return undefined;
    return (
      `<context key="slipfork-world">\n` +
      `Active Slipfork CoW world: ${worldName} (bound to ${sessionDirectory}). ` +
      `PostgreSQL, Redis, S3, app ports, and snow.yml local env vars are isolated to this worktree; run standard shell commands directly.\n` +
      `</context>`
    );
  }
}
