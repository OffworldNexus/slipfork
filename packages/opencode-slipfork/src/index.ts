/**
 * `@slipfork/opencode` OpenCode V2 Server Plugin Composition Root.
 *
 * Wires real `git worktree` and `slipfork` CLI adapters into `SlipforkWorktreeCoordinator`
 * and registers:
 *  1. `ctx.worktree.transform` (automatic CoW world fork on worktree create/remove)
 *  2. `ctx.shell.hook("create.before")` (implicit shell routing by session directory)
 *  3. `ctx.session.hook("context")` (2-line ambient world awareness)
 */
import { execFile } from "node:child_process";
import { join } from "node:path";
import { promisify } from "node:util";

import {
  type GitWorktreeGateway,
  type SessionContextEvent,
  type ShellCreateBeforeEvent,
  type SlipforkGateway,
  SlipforkWorktreeCoordinator,
} from "./coordinator.ts";

const execFileAsync = promisify(execFile);

function shellSingleQuote(text: string): string {
  return `'${text.replaceAll("'", `'\\''`)}'`;
}

class CliSlipforkGateway implements SlipforkGateway {
  constructor(
    private readonly binary: string,
    private readonly canonicalRoot: string,
  ) {}

  async forkWorld(input: {
    worldName: string;
    workspaceDir: string;
    signal: AbortSignal;
  }): Promise<void> {
    await execFileAsync(
      this.binary,
      ["fork", input.worldName, "--workspace", input.workspaceDir],
      { cwd: this.canonicalRoot, signal: input.signal },
    );
  }

  async dropWorld(input: { worldName: string; signal: AbortSignal }): Promise<void> {
    await execFileAsync(this.binary, ["drop", input.worldName], {
      cwd: this.canonicalRoot,
      signal: input.signal,
    });
  }

  wrapShellCommand(input: {
    worldName: string;
    cwd: string;
    shell: string;
    command: string;
  }): { shell: string; command: string; env: Record<string, string> } {
    const snowPath = join(input.cwd, "snow.yml");
    const wrappedCmd = [
      shellSingleQuote(this.binary),
      "exec",
      "--world",
      shellSingleQuote(input.worldName),
      "--cwd",
      shellSingleQuote(input.cwd),
      "--snow-file",
      shellSingleQuote(snowPath),
      "--",
      shellSingleQuote(input.shell),
      "-c",
      shellSingleQuote(input.command),
    ].join(" ");
    return {
      shell: "/bin/sh",
      command: wrappedCmd,
      env: { SLIPFORK_ACTIVE_WORLD: input.worldName },
    };
  }
}

class CliGitWorktreeGateway implements GitWorktreeGateway {
  async add(input: {
    sourceDirectory: string;
    directory: string;
    branch?: string;
    signal: AbortSignal;
  }): Promise<void> {
    const args = ["worktree", "add", input.directory];
    if (input.branch) args.push(input.branch);
    await execFileAsync("git", args, { cwd: input.sourceDirectory, signal: input.signal });
  }

  async remove(input: { directory: string; force: boolean; signal: AbortSignal }): Promise<void> {
    const args = ["worktree", "remove"];
    if (input.force) args.push("--force");
    args.push(input.directory);
    await execFileAsync("git", args, { signal: input.signal });
  }

  async list(
    sourceDirectory: string,
    signal: AbortSignal,
  ): Promise<readonly { directory: string; type: "root" | "worktree" }[]> {
    const { stdout } = await execFileAsync("git", ["worktree", "list", "--porcelain"], {
      cwd: sourceDirectory,
      signal,
    });
    const out: { directory: string; type: "root" | "worktree" }[] = [];
    let idx = 0;
    for (const line of stdout.split("\n")) {
      if (line.startsWith("worktree ")) {
        out.push({
          directory: line.slice("worktree ".length).trim(),
          type: idx === 0 ? "root" : "worktree",
        });
        idx += 1;
      }
    }
    return out;
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
    transform(cb: (editor: { add(strategy: unknown): void }) => void): Promise<{
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
      ctx.location.project?.canonical ??
        ctx.location.project?.directory ??
        ctx.location.directory,
    );
    const binary = typeof ctx.options.binary === "string" ? ctx.options.binary : "slipfork";
    const coordinator = new SlipforkWorktreeCoordinator(
      canonical,
      new CliSlipforkGateway(binary, canonical),
      new CliGitWorktreeGateway(),
    );

    const wtReg = await ctx.worktree.transform((editor) => {
      editor.add(coordinator.makeWorktreeStrategy());
    });
    const shellReg = await ctx.shell.hook("create.before", (event) => {
      coordinator.handleShellBefore(event);
    });
    const ctxReg = await ctx.session.hook("context", async (event) => {
      const info = await ctx.session.get({ sessionID: event.sessionID });
      const dir = info.location?.directory;
      if (!dir) return;
      const hint = coordinator.buildContextHint(dir);
      if (hint) event.system.push({ type: "text", text: hint });
    });

    return async () => {
      await ctxReg.dispose();
      await shellReg.dispose();
      await wtReg.dispose();
    };
  },
};
