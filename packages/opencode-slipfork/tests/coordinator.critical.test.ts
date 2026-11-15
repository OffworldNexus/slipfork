import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import {
  type GitWorktreeGateway,
  type ShellCreateBeforeEvent,
  type SlipforkGateway,
  SlipforkWorktreeCoordinator,
} from "../src/coordinator.ts";
import { CliSlipforkGateway } from "../src/index.ts";

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

  wrapShellCommand(input: {
    worldName: string;
    workspaceDir: string;
    cwd: string;
    shell: string;
    command: string;
  }) {
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

  test("CliSlipforkGateway passes --snow-file only when <worktree>/snow.yml exists, including from subdirectories", () => {
    const cacheBase = join(homedir(), ".cache", "slipfork-critical-tests");
    mkdirSync(cacheBase, { recursive: true });
    const scratch = mkdtempSync(join(cacheBase, "opencode-plugin-"));
    try {
      const gw = new CliSlipforkGateway("slipfork", scratch);

      // 1. Repo/worktree WITHOUT snow.yml (root and subdirectory): --snow-file must be omitted!
      const wtNoSnow = join(scratch, "wt-no-snow");
      const wtNoSnowSub = join(wtNoSnow, "packages", "web");
      mkdirSync(wtNoSnowSub, { recursive: true });

      const noSnowRoot = gw.wrapShellCommand({
        worldName: "wt-no-snow",
        workspaceDir: wtNoSnow,
        cwd: wtNoSnow,
        shell: "/bin/bash",
        command: "bun test",
      });
      expect(noSnowRoot.command).not.toContain("--snow-file");
      expect(noSnowRoot.command).toContain(`--cwd '${wtNoSnow}'`);

      const noSnowSub = gw.wrapShellCommand({
        worldName: "wt-no-snow",
        workspaceDir: wtNoSnow,
        cwd: wtNoSnowSub,
        shell: "/bin/bash",
        command: "bun test",
      });
      expect(noSnowSub.command).not.toContain("--snow-file");
      expect(noSnowSub.command).toContain(`--cwd '${wtNoSnowSub}'`);

      // 2. Worktree WITH snow.yml at worktree root, invoked from a subdirectory:
      //    uses <worktree>/snow.yml and NEVER fabricates <subdir>/snow.yml!
      const wtWithSnow = join(scratch, "wt-with-snow");
      const wtWithSnowSub = join(wtWithSnow, "api", "tests");
      mkdirSync(wtWithSnowSub, { recursive: true });
      const snowFile = join(wtWithSnow, "snow.yml");
      writeFileSync(snowFile, "modes:\n  local: {}\n");

      const withSnowSub = gw.wrapShellCommand({
        worldName: "wt-with-snow",
        workspaceDir: wtWithSnow,
        cwd: wtWithSnowSub,
        shell: "/bin/bash",
        command: "pytest -q",
      });
      expect(withSnowSub.command).toContain(`--snow-file '${snowFile}'`);
      expect(withSnowSub.command).not.toContain(join(wtWithSnowSub, "snow.yml"));
      expect(withSnowSub.command).toContain(`--cwd '${wtWithSnowSub}'`);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });
});
