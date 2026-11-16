/**
 * Protected live end-to-end test (`M2.18`).
 *
 * Drives the real plugin composition root (`default.setup`) against the compiled `slipfork`
 * binary: it auto-spawns `slipforkd`, forks a worktree world, routes a shell command through the
 * world with `$BASE_URL` injected by `slipfork exec`, reaches the same origin from the host, and
 * drops the world on plugin teardown. All state lives on disk under `~/.cache`, never `/tmp`.
 */
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import plugin from "../src/index.ts";
import type {
  ShellCreateBeforeEvent,
  SlipforkWorldRow,
  WorktreeStrategy,
} from "../src/coordinator.ts";

const REPO_ROOT = join(import.meta.dir, "..", "..", "..");
const BINARY = join(REPO_ROOT, "target", "debug", "slipfork");
const PROJECT = "acme-live";
const FORK = "feat-live";

const SNOW_YML = `images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    internal_ports: [8000]
    run_command: "exec python3 -m http.server 8000 --bind 127.0.0.1"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

modes:
  local:
    envs:
      app:
        BASE_URL: '\${{ resources::app.url }}'
`;

const DOCKERFILE = `FROM alpine:3.21 AS app
RUN apk add --no-cache python3
WORKDIR /workspace
`;

function git(cwd: string, args: string[]): void {
  execFileSync("git", ["-c", "user.name=Slipfork Test", "-c", "user.email=test@localhost", ...args], {
    cwd,
    stdio: "ignore",
  });
}

function repoFixture(scratch: string): string {
  const repo = join(scratch, PROJECT);
  mkdirSync(repo, { recursive: true });
  git(repo, ["init", "-q"]);
  writeFileSync(join(repo, "snow.yml"), SNOW_YML);
  writeFileSync(join(repo, "Dockerfile"), DOCKERFILE);
  writeFileSync(join(repo, "slipfork.toml"), `project = "${PROJECT}"\n`);
  git(repo, ["add", "snow.yml", "Dockerfile", "slipfork.toml"]);
  git(repo, ["commit", "-q", "-m", "init"]);
  return repo;
}

interface Registry {
  dispose(): Promise<void>;
}

interface FakeContext {
  options: Record<string, unknown>;
  location: { directory: string; project: { canonical: string; directory: string } };
  session: {
    get(input: { sessionID: string }): Promise<{ location?: { directory?: string } }>;
    hook(name: "context", cb: (event: { sessionID: string; system: { type: "text"; text: string }[] }) => Promise<void> | void): Promise<Registry>;
  };
  worktree: { transform(cb: (editor: { add(strategy: WorktreeStrategy): void }) => void): Promise<Registry> };
  shell: { hook(name: "create.before", cb: (event: ShellCreateBeforeEvent) => Promise<void> | void): Promise<Registry> };
}

describe("opencode-slipfork live world (M2.18)", () => {
  let scratch = "";
  let repo = "";
  let worktree = "";
  let strategy: WorktreeStrategy;
  let shellHook: (event: ShellCreateBeforeEvent) => Promise<void> | void;
  let contextHook: (event: { sessionID: string; system: { type: "text"; text: string }[] }) => Promise<void> | void;
  let sessionDirectory = "";
  let dispose: (() => Promise<void>) | undefined;
  let savedEnv: { XDG_CACHE_HOME?: string; XDG_DATA_HOME?: string; XDG_RUNTIME_DIR?: string; BROWSER?: string };

  const childEnv = (): NodeJS.ProcessEnv => ({ ...process.env });

  function cli(args: string[]): string {
    return execFileSync(BINARY, args, {
      cwd: repo,
      encoding: "utf8",
      env: childEnv(),
    });
  }

  function listWorlds(): SlipforkWorldRow[] {
    return JSON.parse(cli(["--json", "list"])) as SlipforkWorldRow[];
  }

  beforeAll(async () => {
    if (!existsSync(BINARY)) {
      execFileSync("cargo", ["build", "-p", "slipfork", "--bins"], { cwd: REPO_ROOT, stdio: "inherit" });
    }
    const worldInit = join(REPO_ROOT, "target", "debug", "slipfork-world-init");
    if (!existsSync(worldInit)) {
      execFileSync("cargo", ["build", "-p", "slipfork-engine", "--bins"], {
        cwd: REPO_ROOT,
        stdio: "inherit",
      });
    }

    // AF_UNIX paths (world `netns.sock`) have a 107-byte budget, so the scratch root stays short
    // while remaining on disk (never `/tmp` tmpfs).
    const cacheBase = join(homedir(), ".cache");
    mkdirSync(cacheBase, { recursive: true });
    scratch = mkdtempSync(join(cacheBase, "sf-e2e-"));
    repo = repoFixture(scratch);

    savedEnv = {
      XDG_CACHE_HOME: process.env.XDG_CACHE_HOME,
      XDG_DATA_HOME: process.env.XDG_DATA_HOME,
      XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR,
      BROWSER: process.env.BROWSER,
    };
    process.env.XDG_CACHE_HOME = join(scratch, "xdg-cache");
    process.env.XDG_DATA_HOME = join(scratch, "xdg-data");
    process.env.BROWSER = "/bin/true";
    // The daemon runtime keeps living on disk under `<state_dir>/run`; the plugin itself never
    // reads the environment, the `slipfork` composition root does.
    delete process.env.XDG_RUNTIME_DIR;

    const ctx: FakeContext = {
      options: { binary: BINARY },
      location: { directory: repo, project: { canonical: repo, directory: repo } },
      session: {
        get: async () => ({ location: { directory: sessionDirectory } }),
        hook: async (_name, cb) => {
          contextHook = cb;
          return { dispose: async () => {} };
        },
      },
      worktree: {
        transform: async (cb) => {
          cb({ add: (registered) => (strategy = registered) });
          return { dispose: async () => {} };
        },
      },
      shell: {
        hook: async (_name, cb) => {
          shellHook = cb;
          return { dispose: async () => {} };
        },
      },
    };
    dispose = (await plugin.setup(ctx)) as unknown as () => Promise<void>;
    const created = await strategy.create(
      { sourceDirectory: repo, directory: join(repo, ".worktrees", FORK) },
      { signal: new AbortController().signal },
    );
    worktree = created.directory;
  }, 300_000);

  afterAll(async () => {
    if (dispose !== undefined) await dispose().catch(() => {});
    if (scratch === "") return;
    // Reap the on-demand daemon this test auto-spawned (the CLI keeps no pid file).
    const runtime = join(scratch, "xdg-cache", "slipfork", "run");
    for (const pid of readdirSync("/proc")) {
      if (!/^\d+$/.test(pid)) continue;
      try {
        const cmdline = readFileSync(`/proc/${pid}/cmdline`, "utf8");
        if (cmdline.includes("slipforkd") && cmdline.includes(runtime)) process.kill(Number(pid), "SIGTERM");
      } catch {
        // The process exited between the directory read and the command-line read.
      }
    }
    if (savedEnv.XDG_CACHE_HOME === undefined) delete process.env.XDG_CACHE_HOME;
    else process.env.XDG_CACHE_HOME = savedEnv.XDG_CACHE_HOME;
    if (savedEnv.XDG_DATA_HOME === undefined) delete process.env.XDG_DATA_HOME;
    else process.env.XDG_DATA_HOME = savedEnv.XDG_DATA_HOME;
    if (savedEnv.XDG_RUNTIME_DIR === undefined) delete process.env.XDG_RUNTIME_DIR;
    else process.env.XDG_RUNTIME_DIR = savedEnv.XDG_RUNTIME_DIR;
    if (savedEnv.BROWSER === undefined) delete process.env.BROWSER;
    else process.env.BROWSER = savedEnv.BROWSER;
    rmSync(scratch, { recursive: true, force: true });
  }, 120_000);

  test("forking through the plugin creates the CLI-owned worktree world", () => {
    expect(worktree).toBe(join(repo, ".worktrees", FORK));
    expect(existsSync(join(worktree, "snow.yml"))).toBe(true);

    const rows = listWorlds();
    const fork = rows.find((row) => row.worktree === worktree);
    expect(fork).toBeDefined();
    expect(fork?.world.endsWith(`:${FORK}`)).toBe(true);
    expect(fork?.origin).toMatch(new RegExp(`^http://${FORK}\\.${PROJECT}\\.localhost:\\d+$`));
  });

  test("shell commands run inside the world cwd with the daemon-injected $BASE_URL", () => {
    const origin = listWorlds().find((row) => row.worktree === worktree)?.origin as string;
    const nested = join(worktree, "api");
    mkdirSync(nested, { recursive: true });

    const event: ShellCreateBeforeEvent = {
      command: 'printf "%s\\n%s" "$PWD" "$BASE_URL"',
      cwd: nested,
      timeout: 60_000,
      shell: "/bin/sh",
      env: {},
    };
    const outcome = shellHook(event);
    expect(outcome).toBeUndefined();
    expect(event.shell).toBe("/bin/sh");
    expect(event.command).toContain(`'exec' '--world' '${FORK}'`);
    expect(event.command).not.toContain("snow.yml");
    // The plugin decided nothing about the world environment: only the route marker is set.
    expect(event.env).toEqual({ SLIPFORK_ACTIVE_WORLD: FORK });

    const env: NodeJS.ProcessEnv = { ...childEnv() };
    for (const [key, value] of Object.entries(event.env)) {
      if (value !== undefined) env[key] = value;
    }
    const [pwd, baseUrl] = execFileSync(event.shell, ["-c", event.command], {
      encoding: "utf8",
      env,
    }).split("\n");
    expect(pwd).toBe(nested);
    expect(baseUrl).toBe(origin);
  }, 120_000);

  test("the session context names the world and its single origin URL", async () => {
    const origin = listWorlds().find((row) => row.worktree === worktree)?.origin as string;
    sessionDirectory = worktree;
    const event = { sessionID: "ses_live", system: [] as { type: "text"; text: string }[] };
    await contextHook(event);
    expect(event.system).toHaveLength(1);
    const hint = event.system[0]?.text ?? "";
    expect(hint).toContain('<context key="slipfork-world">');
    expect(hint).toContain(FORK);
    expect(hint).toContain(origin);
    expect(hint).toContain(":5432");
  });

  test("the same world URL is reachable from the host through the daemon ingress", async () => {
    const origin = listWorlds().find((row) => row.worktree === worktree)?.origin as string;
    const response = await fetch(origin);
    expect(response.status).toBe(200);
    expect(await response.text()).toContain("Directory listing");
  }, 60_000);

  test("session teardown drops the world once and a second dispose is a no-op", async () => {
    await dispose?.();
    expect(existsSync(worktree)).toBe(false);
    expect(listWorlds().some((row) => row.worktree === worktree)).toBe(false);

    // Idempotent: the second teardown neither throws nor re-runs the CLI drop.
    await expect(dispose?.()).resolves.toBeUndefined();
    expect(existsSync(worktree)).toBe(false);
  }, 120_000);
});
