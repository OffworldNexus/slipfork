import { describe, expect, test } from "bun:test";
import {
  type GitWorktreeGateway,
  type ShellCreateBeforeEvent,
  type SlipforkGateway,
  SlipforkWorktreeCoordinator,
} from "../src/coordinator.ts";

class FakeSlipfork implements SlipforkGateway {
  readonly forked: string[] = [];
  readonly dropped: string[] = [];
  failNextFork = false;

  async forkWorld(input: { worldName: string; workspaceDir: string }): Promise<void> {
    if (this.failNextFork) {
      this.failNextFork = false;
      throw new Error("FICLONE unsupported");
    }
    this.forked.push(`${input.worldName}@${input.workspaceDir}`);
  }

  async dropWorld(input: { worldName: string }): Promise<void> {
    this.dropped.push(input.worldName);
  }

  wrapShellCommand(input: { worldName: string; cwd: string; shell: string; command: string }) {
    return {
      shell: "/bin/sh",
      command: `slipfork exec --world ${input.worldName} --cwd ${input.cwd} -- ${input.shell} -c '${input.command}'`,
      env: { SLIPFORK_ACTIVE_WORLD: input.worldName },
    };
  }
}

class FakeGit implements GitWorktreeGateway {
  readonly worktrees = new Set<string>();

  async add(input: { directory: string }): Promise<void> {
    this.worktrees.add(input.directory);
  }

  async remove(input: { directory: string }): Promise<void> {
    this.worktrees.delete(input.directory);
  }

  async list(sourceDirectory: string) {
    return [
      { directory: sourceDirectory, type: "root" as const },
      ...[...this.worktrees].map((d) => ({ directory: d, type: "worktree" as const })),
    ];
  }
}

describe("SlipforkWorktreeCoordinator (R13)", () => {
  test("creates CoW world on worktree.create, routes shell implicitly by cwd, and cleans up on remove", async () => {
    const sf = new FakeSlipfork();
    const git = new FakeGit();
    const coord = new SlipforkWorktreeCoordinator("/repo", sf, git);
    const strategy = coord.makeWorktreeStrategy();
    const signal = new AbortController().signal;

    const created = await strategy.create(
      { sourceDirectory: "/repo", directory: "/repo/.worktrees/feat-login" },
      { signal },
    );
    expect(created.directory).toBe("/repo/.worktrees/feat-login");
    expect(sf.forked).toEqual(["wt-feat-login@/repo/.worktrees/feat-login"]);

    // Shell in worktree subdirectory routes into wt-feat-login automatically!
    const shellEvt: ShellCreateBeforeEvent = {
      command: "pytest -q",
      cwd: "/repo/.worktrees/feat-login/api",
      timeout: 60_000,
      shell: "/bin/bash",
      env: {},
    };
    coord.handleShellBefore(shellEvt);
    expect(shellEvt.command).toContain("slipfork exec --world wt-feat-login");
    expect(shellEvt.env.SLIPFORK_ACTIVE_WORLD).toBe("wt-feat-login");

    // Unrelated directory stays untouched.
    const hostEvt: ShellCreateBeforeEvent = {
      command: "git status",
      cwd: "/other/dir",
      timeout: 10_000,
      shell: "/bin/bash",
      env: {},
    };
    coord.handleShellBefore(hostEvt);
    expect(hostEvt.command).toBe("git status");

    const hint = coord.buildContextHint("/repo/.worktrees/feat-login");
    expect(hint).toContain("wt-feat-login");

    await strategy.remove({ directory: "/repo/.worktrees/feat-login", force: false }, { signal });
    expect(sf.dropped).toEqual(["wt-feat-login"]);
    expect(git.worktrees.size).toBe(0);
  });

  test("rolls back git worktree cleanly when CoW fork fails closed", async () => {
    const sf = new FakeSlipfork();
    sf.failNextFork = true;
    const git = new FakeGit();
    const coord = new SlipforkWorktreeCoordinator("/repo", sf, git);
    const strategy = coord.makeWorktreeStrategy();

    await expect(
      strategy.create(
        { sourceDirectory: "/repo", directory: "/repo/.worktrees/broken" },
        { signal: new AbortController().signal },
      ),
    ).rejects.toThrow("FICLONE unsupported");
    expect(git.worktrees.size).toBe(0);
  });
});
