import { describe, expect, test } from "bun:test";
import {
  type ForkedWorld,
  type ManifestProbe,
  type ShellCreateBeforeEvent,
  type SlipforkGateway,
  type SlipforkProjectState,
  type SlipforkWorldRow,
  SlipforkWorktreeCoordinator,
  loadSlipforkProjectState,
} from "../src/coordinator.ts";
import { CliSlipforkGateway } from "../src/index.ts";

const REPO = "/repo/acme-app";
const MANIFEST: SlipforkProjectState = {
  repoRoot: REPO,
  snowFile: `${REPO}/snow.yml`,
};

const FORK = `${REPO}/.worktrees/feat-login`;
const ORIGIN = "http://feat-login.acme-app.localhost:3000";

class FakeSlipfork implements SlipforkGateway {
  readonly forked: { fork: string; base?: string }[] = [];
  readonly dropped: string[] = [];
  readonly rows: SlipforkWorldRow[] = [];
  failNextFork = false;
  failNextDrop = false;

  async forkWorld(input: { fork: string; base?: string }): Promise<ForkedWorld> {
    if (this.failNextFork) {
      this.failNextFork = false;
      throw new Error("slipfork fork 'feat-login' failed: FICLONE unsupported");
    }
    this.forked.push({ fork: input.fork, base: input.base });
    return { world: `acme-app:${input.fork}`, origin: ORIGIN };
  }

  async dropWorld(input: { fork: string }): Promise<void> {
    if (this.failNextDrop) {
      this.failNextDrop = false;
      throw new Error("cannot reach slipforkd");
    }
    this.dropped.push(input.fork);
  }

  async listWorlds(): Promise<readonly SlipforkWorldRow[]> {
    return this.rows;
  }

  wrapShellCommand(input: {
    fork: string;
    cwd: string;
    shell: string;
    command: string;
  }): { shell: string; command: string; env: Record<string, string> } {
    return {
      shell: "/bin/sh",
      command: `slipfork exec --world '${input.fork}' --cwd '${input.cwd}' -- ${input.shell} -c '${input.command}'`,
      env: { SLIPFORK_ACTIVE_WORLD: input.fork },
    };
  }
}

function shellEvent(cwd: string, command = "pytest -q"): ShellCreateBeforeEvent {
  return { command, cwd, timeout: 60_000, shell: "/bin/bash", env: {} };
}

async function forkedCoordinator(
  project: SlipforkProjectState = MANIFEST,
): Promise<{ coordinator: SlipforkWorktreeCoordinator; slipfork: FakeSlipfork }> {
  const slipfork = new FakeSlipfork();
  const coordinator = new SlipforkWorktreeCoordinator(project, slipfork);
  const strategy = coordinator.makeWorktreeStrategy();
  await strategy.create(
    { sourceDirectory: REPO, directory: FORK, branch: "feat-login" },
    { signal: new AbortController().signal },
  );
  return { coordinator, slipfork };
}

describe("SlipforkWorktreeCoordinator (ADR-0004, M2.18)", () => {
  test("forks through the CLI, routes shell commands by cwd, and drops idempotently", async () => {
    const slipfork = new FakeSlipfork();
    const coordinator = new SlipforkWorktreeCoordinator(MANIFEST, slipfork);
    const strategy = coordinator.makeWorktreeStrategy();
    const signal = new AbortController().signal;

    const created = await strategy.create(
      { sourceDirectory: REPO, directory: FORK, branch: "feat-login" },
      { signal },
    );
    expect(created.directory).toBe(FORK);
    expect(slipfork.forked).toEqual([{ fork: "feat-login", base: "feat-login" }]);

    // Shell in a worktree subdirectory routes into the world.
    const routed = shellEvent(`${FORK}/api`);
    const outcome = coordinator.handleShellBefore(routed);
    expect(outcome).toEqual({ kind: "routed", forkId: "feat-login", cwd: `${FORK}/api`, url: ORIGIN });
    expect(routed.command).toContain("slipfork exec --world 'feat-login'");
    expect(routed.command).toContain(`--cwd '${FORK}/api'`);
    expect(routed.env.SLIPFORK_ACTIVE_WORLD).toBe("feat-login");

    // The canonical checkout stays on the host.
    const canonical = shellEvent(`${REPO}/api`);
    expect(coordinator.handleShellBefore(canonical)).toEqual({
      kind: "untouched",
      reason: "unmanaged-checkout",
    });
    expect(canonical.command).toBe("pytest -q");

    // Unrelated directories are never touched.
    const outside = shellEvent("/elsewhere");
    expect(coordinator.handleShellBefore(outside)).toEqual({
      kind: "untouched",
      reason: "outside-project",
    });

    const hint = coordinator.buildContextHint(`${FORK}/api`);
    expect(hint).toContain(`<context key="slipfork-world">`);
    expect(hint).toContain("feat-login");
    expect(hint).toContain(ORIGIN);
    expect(hint).toContain(":5432");

    await strategy.remove({ directory: FORK, force: false }, { signal });
    expect(slipfork.dropped).toEqual(["feat-login"]);
    expect(coordinator.buildContextHint(FORK)).toBeUndefined();

    // A second drop for the same session is a no-op, never a second CLI call.
    await strategy.remove({ directory: FORK, force: false }, { signal });
    expect(slipfork.dropped).toEqual(["feat-login"]);
  });

  test("a failed fork binds nothing and leaves no world behind", async () => {
    const slipfork = new FakeSlipfork();
    slipfork.failNextFork = true;
    const coordinator = new SlipforkWorktreeCoordinator(MANIFEST, slipfork);
    const strategy = coordinator.makeWorktreeStrategy();

    await expect(
      strategy.create(
        { sourceDirectory: REPO, directory: FORK },
        { signal: new AbortController().signal },
      ),
    ).rejects.toThrow("FICLONE unsupported");
    expect(coordinator.bindingForDirectory(FORK)).toBeUndefined();
    expect(slipfork.dropped).toEqual([]);
  });

  test("a missing manifest yields the structured no-manifest result, never a silent skip", () => {
    const slipfork = new FakeSlipfork();
    const coordinator = new SlipforkWorktreeCoordinator(
      { repoRoot: REPO, snowFile: null },
      slipfork,
    );

    const event = shellEvent(`${FORK}/api`);
    const outcome = coordinator.handleShellBefore(event);
    expect(outcome.kind).toBe("no-manifest");
    expect(event.command).toBe("pytest -q");
    expect(event.env).toEqual({});
    expect(coordinator.projectNotice()?.kind).toBe("no-manifest");
    expect(coordinator.buildProjectNoticeHint()).toContain("no snow.yml found");
    expect(coordinator.buildProjectNoticeHint()).toContain(REPO);

    // Outside the project the shell still passes through untouched.
    expect(coordinator.handleShellBefore(shellEvent("/elsewhere"))).toEqual({
      kind: "untouched",
      reason: "outside-project",
    });
  });

  test("an unreadable manifest is a structured config error", () => {
    const coordinator = new SlipforkWorktreeCoordinator(
      { repoRoot: REPO, snowFile: null, configError: "cannot inspect /repo/acme-app/snow.yml: EACCES" },
      new FakeSlipfork(),
    );
    const outcome = coordinator.handleShellBefore(shellEvent(FORK));
    expect(outcome.kind).toBe("config-error");
    expect(outcome.kind === "config-error" && outcome.message).toContain("EACCES");
    expect(coordinator.buildProjectNoticeHint()).toContain("Slipfork cannot read");
  });

  test("plugin teardown releases every world once, is idempotent, and records failures", async () => {
    const slipfork = new FakeSlipfork();
    const coordinator = new SlipforkWorktreeCoordinator(MANIFEST, slipfork);
    const strategy = coordinator.makeWorktreeStrategy();
    const signal = new AbortController().signal;
    await strategy.create({ sourceDirectory: REPO, directory: FORK }, { signal });

    expect(await coordinator.releaseAllWorlds(signal)).toEqual([]);
    expect(await coordinator.releaseAllWorlds(signal)).toEqual([]);
    expect(slipfork.dropped).toEqual(["feat-login"]);
  });

  test("a failed drop is a warning from teardown, never a thrown cleanup error", async () => {
    const slipfork = new FakeSlipfork();
    const coordinator = new SlipforkWorktreeCoordinator(MANIFEST, slipfork);
    const strategy = coordinator.makeWorktreeStrategy();
    const signal = new AbortController().signal;
    await strategy.create({ sourceDirectory: REPO, directory: FORK }, { signal });
    slipfork.failNextDrop = true;

    const warnings = await coordinator.releaseAllWorlds(signal);
    expect(warnings).toEqual(["cannot drop world 'feat-login': cannot reach slipforkd"]);
  });

  test("worktree list reports the root plus only this project's CLI-owned checkouts", async () => {
    const slipfork = new FakeSlipfork();
    slipfork.rows.push(
      { world: "acme-app:feat-login", origin: ORIGIN, worktree: FORK },
      { world: "acme-app:other", origin: "http://other.acme-app.localhost:3000", worktree: "/other/.worktrees/x" },
      { world: "acme-app", origin: "http://acme-app.localhost:3000", worktree: REPO },
    );
    const coordinator = new SlipforkWorktreeCoordinator(MANIFEST, slipfork);
    const listed = await coordinator.makeWorktreeStrategy().list(REPO, {
      signal: new AbortController().signal,
    });
    expect(listed).toEqual([
      { directory: REPO, type: "root" },
      { directory: FORK, type: "worktree" },
    ]);
  });
});

describe("loadSlipforkProjectState (M2.18)", () => {
  const probe = (answer: "present" | "missing" | { error: string }): ManifestProbe => ({
    inspect: () => answer,
  });

  test("records the manifest path when present", () => {
    expect(loadSlipforkProjectState({ repoRoot: REPO, probe: probe("present") })).toEqual({
      repoRoot: REPO,
      snowFile: `${REPO}/snow.yml`,
    });
  });

  test("records a missing manifest explicitly", () => {
    expect(loadSlipforkProjectState({ repoRoot: REPO, probe: probe("missing") })).toEqual({
      repoRoot: REPO,
      snowFile: null,
    });
  });

  test("records an inspection error as configError", () => {
    expect(
      loadSlipforkProjectState({ repoRoot: REPO, probe: probe({ error: "EACCES" }) }),
    ).toEqual({ repoRoot: REPO, snowFile: null, configError: "EACCES" });
  });
});

describe("CliSlipforkGateway command line (M2.18)", () => {
  test("routes through the real CLI surface and never injects snow.yml env", () => {
    const gateway = new CliSlipforkGateway({ binary: "slipfork", repoRoot: REPO });
    const wrapped = gateway.wrapShellCommand({
      fork: "feat-login",
      cwd: `${FORK}/api`,
      shell: "/bin/bash",
      command: "bun test",
    });
    expect(wrapped.shell).toBe("/bin/sh");
    expect(wrapped.command).toContain("'exec' '--world' 'feat-login'");
    expect(wrapped.command).toContain(`'--cwd' '${FORK}/api'`);
    expect(wrapped.command).toContain(`'--' '/bin/bash' '-c' 'bun test'`);
    expect(wrapped.command).not.toContain("--snow-file");
    expect(wrapped.command).not.toContain("snow.yml");
    expect(wrapped.env).toEqual({ SLIPFORK_ACTIVE_WORLD: "feat-login" });
  });
});
