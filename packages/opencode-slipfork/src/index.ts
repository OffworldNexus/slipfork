/**
 * `@slipfork/opencode` OpenCode V2 Server Plugin Composition Root.
 *
 * Wires the real `slipfork` CLI adapter and the one-shot manifest probe into
 * `SlipforkWorktreeCoordinator`, then registers:
 *  1. `ctx.worktree.transform` (automatic CoW world fork on worktree create/remove)
 *  2. `ctx.shell.hook("create.before")` (implicit routing through `slipfork exec`)
 *  3. `ctx.session.hook("context")` (world + single-origin awareness)
 *
 * This module is the only place that touches the host filesystem, `process.env`, or child
 * processes (`ADR-0004`, `H-010` #1/#2); the coordinator itself stays pure.
 */
import { execFile } from "node:child_process";
import { statSync } from "node:fs";
import { promisify } from "node:util";

import {
  buildDropArgs,
  buildExecCommandLine,
  buildForkArgs,
  buildStatusArgs,
  decodeCliFailure,
  type ForkedWorld,
  parseForkedWorld,
  parseStatusWorlds,
  type SlipforkWorldRow,
} from "./cli.ts";
import {
  type ManifestProbe,
  type SessionContextEvent,
  type ShellCreateBeforeEvent,
  type SlipforkGateway,
  SlipforkWorktreeCoordinator,
  loadSlipforkProjectState,
  type WorktreeStrategy,
} from "./coordinator.ts";

const execFileAsync = promisify(execFile);

/** The host filesystem adapter the explicit project-state loader needs (`M2.18`). */
class StatManifestProbe implements ManifestProbe {
  inspect(path: string): "present" | "missing" | { readonly error: string } {
    try {
      return statSync(path).isFile() ? "present" : { error: `${path} is not a regular file` };
    } catch (err) {
      const code = (err as NodeJS.ErrnoException).code;
      if (code === "ENOENT") return "missing";
      return { error: `cannot inspect ${path}: ${(err as Error).message}` };
    }
  }
}

export interface CliSlipforkGatewayOptions {
  /** The `slipfork` executable (`options.binary`, defaulting to `PATH`). */
  readonly binary: string;
  /** Repository root every command runs from; the daemon resolves worlds from it. */
  readonly repoRoot: string;
}

/** Thin `slipfork` CLI adapter: it only forwards argv and decodes JSON (`ADR-0004`). */
export class CliSlipforkGateway implements SlipforkGateway {
  constructor(private readonly options: CliSlipforkGatewayOptions) {}

  async forkWorld(input: {
    fork: string;
    base?: string;
    signal: AbortSignal;
  }): Promise<ForkedWorld> {
    const stdout = await this.run(buildForkArgs(input), input.signal, `fork '${input.fork}'`);
    return parseForkedWorld(stdout, input.fork);
  }

  async dropWorld(input: { fork: string; signal: AbortSignal }): Promise<void> {
    await this.run(buildDropArgs(input.fork), input.signal, `drop '${input.fork}'`);
  }

  async listWorlds(input: { signal: AbortSignal }): Promise<readonly SlipforkWorldRow[]> {
    const stdout = await this.run(buildStatusArgs(), input.signal, "status");
    return parseStatusWorlds(stdout);
  }

  wrapShellCommand(input: {
    fork: string;
    cwd: string;
    shell: string;
    command: string;
  }): { shell: string; command: string; env: Record<string, string> } {
    // `slipforkd` is the sole owner of the world environment; the plugin only prepends the route
    // and marks the host-side process with the active fork.
    return {
      shell: "/bin/sh",
      command: buildExecCommandLine(this.options.binary, {
        world: input.fork,
        cwd: input.cwd,
        shell: input.shell,
        command: input.command,
      }),
      env: { SLIPFORK_ACTIVE_WORLD: input.fork },
    };
  }

  private async run(
    args: readonly string[],
    signal: AbortSignal,
    what: string,
  ): Promise<string> {
    try {
      const { stdout } = await execFileAsync(this.options.binary, args, {
        cwd: this.options.repoRoot,
        signal,
      });
      return stdout;
    } catch (err) {
      const failure = err as Error & { stdout?: string | Buffer; stderr?: string | Buffer };
      const asText = (value: string | Buffer | undefined): string =>
        typeof value === "string" ? value.trim() : "";
      const stdout = asText(failure.stdout);
      const stderr = asText(failure.stderr);
      // `slipfork --json` prints `{"ok":false,"error":…,"next":…}` on stdout and exits non-zero:
      // surface that one actionable line (plus any `next:` remedy), never a raw JSON blob.
      const plain = [stdout, stderr].filter((part) => part !== "").join("\n");
      const detail = decodeCliFailure(stdout) ?? (plain !== "" ? plain : failure.message);
      throw new Error(`slipfork ${what} failed: ${detail}`);
    }
  }
}

interface OpenCodePluginContext {
  options: Record<string, unknown>;
  location: {
    directory: string;
    project?: { canonical?: string; directory?: string };
  };
  session: {
    get(input: { sessionID: string }): Promise<{ location?: { directory?: string } }>;
    hook(
      name: "context",
      cb: (event: SessionContextEvent) => Promise<void> | void,
    ): Promise<{ dispose(): Promise<void> }>;
  };
  worktree: {
    transform(cb: (editor: { add(strategy: WorktreeStrategy): void }) => void): Promise<{
      dispose(): Promise<void>;
    }>;
  };
  shell: {
    hook(
      name: "create.before",
      cb: (event: ShellCreateBeforeEvent) => Promise<void> | void,
    ): Promise<{ dispose(): Promise<void> }>;
  };
}

export { SlipforkWorktreeCoordinator } from "./coordinator.ts";

export default {
  id: "slipfork.opencode",
  async setup(ctx: OpenCodePluginContext) {
    const canonical = String(
      ctx.location.project?.canonical ?? ctx.location.project?.directory ?? ctx.location.directory,
    );
    const binary = typeof ctx.options.binary === "string" ? ctx.options.binary : "slipfork";
    const project = loadSlipforkProjectState({
      repoRoot: canonical,
      probe: new StatManifestProbe(),
    });
    const coordinator = new SlipforkWorktreeCoordinator(
      project,
      new CliSlipforkGateway({ binary, repoRoot: canonical }),
    );

    const notice = coordinator.projectNotice();
    if (notice !== undefined) console.warn(`slipfork: ${notice.message}`);

    const wtReg = await ctx.worktree.transform((editor) => {
      editor.add(coordinator.makeWorktreeStrategy());
    });
    const shellReg = await ctx.shell.hook("create.before", (event) => {
      coordinator.handleShellBefore(event);
    });
    const ctxReg = await ctx.session.hook("context", async (event) => {
      const info = await ctx.session.get({ sessionID: event.sessionID });
      const dir = info.location?.directory;
      if (dir === undefined) return;
      const hint = coordinator.buildContextHint(dir) ?? coordinator.buildProjectNoticeHint();
      if (hint !== undefined) event.system.push({ type: "text", text: hint });
    });

    return async () => {
      await ctxReg.dispose();
      await shellReg.dispose();
      await wtReg.dispose();
      // Teardown never throws: failed releases are recorded as warnings for the operator.
      for (const warning of await coordinator.releaseAllWorlds(new AbortController().signal)) {
        console.warn(`slipfork: ${warning}`);
      }
    };
  },
};
