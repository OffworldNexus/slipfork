/**
 * Protected live end-to-end test (`M2.18`), dual-mode like the Rust live suites (`M3.15`).
 *
 * Drives the real plugin composition root (`default.setup`) against the compiled `slipfork`
 * binary: it auto-spawns `slipforkd`, forks a worktree world, routes a shell command through the
 * world with `$BASE_URL` injected by `slipfork exec`, reaches the same origin from the host, and
 * drops the world on plugin teardown. All state lives on disk under `~/.cache`, never `/tmp`.
 *
 * - **Btrfs state dir:** the full world flow below runs, with every assertion.
 * - **Non-Btrfs state dir:** the production `slipforkd` boot gate must refuse the fork with
 *   `EngineError::BtrfsRequired`, surfacing the exact `user_subvol_rm_allowed,noatime` mount hint
 *   in the daemon log the CLI names, and leaving no world, checkout, or daemon behind. The full
 *   flow runs on the Btrfs VM.
 *
 * No test is skipped, ignored, or weakened in either mode.
 */
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statfsSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type { SlipforkWorldRow } from "../src/cli.ts";
import plugin from "../src/index.ts";
import type {
  ShellCreateBeforeEvent,
  WorktreeStrategy,
} from "../src/coordinator.ts";

const REPO_ROOT = join(import.meta.dir, "..", "..", "..");
const BINARY = join(REPO_ROOT, "target", "debug", "slipfork");
const PROJECT = "acme-live";
const FORK = "feat-live";

/** `BTRFS_SUPER_MAGIC`, the same `statfs(2)` value the daemon's boot gate compares. */
const BTRFS_SUPER_MAGIC = 0x9123_683e;

/** The single `EngineError::BtrfsRequired` diagnostic contract (mirrors `support::assert_btrfs_refusal`). */
const BTRFS_REFUSAL_NEEDLES = [
  "is not on Btrfs",
  "0x9123683e",
  "BTRFS_SUPER_MAGIC",
  "user_subvol_rm_allowed,noatime",
] as const;

/**
 * `true` when the disk-backed scratch root (`~/.cache`, the parent of this suite's state dir)
 * sits on Btrfs — the single condition `slipforkd` gates a boot on.
 */
function hostIsBtrfs(): boolean {
  const cacheBase = join(homedir(), ".cache");
  mkdirSync(cacheBase, { recursive: true });
  return (statfsSync(cacheBase).type >>> 0) === BTRFS_SUPER_MAGIC;
}

const LIVE = hostIsBtrfs();

/** Every live `slipforkd` pid whose command line names this suite's runtime directory. */
function runningDaemons(runtime: string): number[] {
  const pids: number[] = [];
  for (const pid of readdirSync("/proc")) {
    if (!/^\d+$/.test(pid)) continue;
    try {
      const cmdline = readFileSync(`/proc/${pid}/cmdline`, "utf8");
      if (cmdline.includes("slipforkd") && cmdline.includes(runtime)) pids.push(Number(pid));
    } catch {
      // The process exited between the directory read and the command-line read.
    }
  }
  return pids;
}

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
  let savedEnv: { XDG_CACHE_HOME?: string; XDG_DATA_HOME?: string; XDG_RUNTIME_DIR?: string };
  /** The refusal the production daemon surfaced on a non-Btrfs state dir. */
  let forkError: Error | undefined;

  const childEnv = (): NodeJS.ProcessEnv => ({ ...process.env });

  function cli(args: string[]): string {
    return execFileSync(BINARY, args, {
      cwd: repo,
      encoding: "utf8",
      env: childEnv(),
    });
  }

  /** The one `slipfork --json status` document (the CLI's whole-fleet read). */
  function statusDocument(): { ok: boolean; worlds: SlipforkWorldRow[] } {
    return JSON.parse(cli(["--json", "status"])) as {
      ok: boolean;
      worlds: SlipforkWorldRow[];
    };
  }

  function listWorlds(): SlipforkWorldRow[] {
    return statusDocument().worlds;
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
    };
    process.env.XDG_CACHE_HOME = join(scratch, "xdg-cache");
    process.env.XDG_DATA_HOME = join(scratch, "xdg-data");
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
    if (!LIVE) {
      // The plugin still registers the real strategy; the refusal comes from the production
      // daemon boot gate, never from the plugin deciding to skip a world.
      expect(typeof strategy.create).toBe("function");
      try {
        await strategy.create(
          { sourceDirectory: repo, directory: join(repo, ".worktrees", FORK) },
          { signal: new AbortController().signal },
        );
      } catch (error) {
        forkError = error as Error;
      }
      return;
    }
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
    for (const pid of runningDaemons(runtime)) process.kill(pid, "SIGTERM");
    if (savedEnv.XDG_CACHE_HOME === undefined) delete process.env.XDG_CACHE_HOME;
    else process.env.XDG_CACHE_HOME = savedEnv.XDG_CACHE_HOME;
    if (savedEnv.XDG_DATA_HOME === undefined) delete process.env.XDG_DATA_HOME;
    else process.env.XDG_DATA_HOME = savedEnv.XDG_DATA_HOME;
    if (savedEnv.XDG_RUNTIME_DIR === undefined) delete process.env.XDG_RUNTIME_DIR;
    else process.env.XDG_RUNTIME_DIR = savedEnv.XDG_RUNTIME_DIR;
    rmSync(scratch, { recursive: true, force: true });
  }, 120_000);

  if (LIVE) {
    test("piped human status stays ANSI-free and --json carries the fleet document", () => {
      // The daemon is up and the fork exists; `status` renders the real fleet tree
      // (`TerminalUi` built off a non-TTY), so any stray escape byte or spinner would fail here.
      const human = cli(["status"]);
      expect(human).toContain(FORK);
      expect(human).toContain(worktree);
      expect(human).not.toContain("\x1b");

      const document = statusDocument();
      expect(document.ok).toBe(true);
      expect(Array.isArray(document.worlds)).toBe(true);
      expect(document.worlds.some((row) => row.worktree === worktree)).toBe(true);
    });

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
      expect(hint).not.toContain("\x1b");
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
  } else {
    test("a non-Btrfs state dir refuses the world with BtrfsRequired", () => {
      // The plugin routed the fork through the production `slipfork` CLI, whose auto-spawned
      // `slipforkd` refused the boot before any world existed. The CLI names the real daemon log...
      expect(forkError).toBeInstanceOf(Error);
      const message = (forkError as Error).message;
      expect(message).toContain(`slipfork fork '${FORK}' failed:`);
      const stateDir = join(scratch, "xdg-cache", "slipfork");
      const runtime = join(stateDir, "run");
      const logPath = join(runtime, "daemon.log");
      expect(message).toContain(logPath);
      // ...which carries the exact `EngineError::BtrfsRequired` diagnostic and mount hint.
      const diagnostic = readFileSync(logPath, "utf8");
      for (const needle of BTRFS_REFUSAL_NEEDLES) {
        expect(diagnostic).toContain(needle);
      }
      expect(diagnostic.toLowerCase()).not.toContain("reflink");

      // Fail closed: no world, checkout, control socket, or daemon survives the refusal.
      expect(existsSync(join(stateDir, "worlds"))).toBe(false);
      expect(existsSync(join(runtime, "daemon.sock"))).toBe(false);
      expect(existsSync(join(repo, ".worktrees", FORK))).toBe(false);
      expect(runningDaemons(runtime)).toEqual([]);

      console.warn(
        "non-Btrfs state dir: the full opencode-slipfork world flow runs on the Btrfs VM",
      );
    });
  }
});
