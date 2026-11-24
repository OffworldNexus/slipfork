/**
 * Lab journeys against the real `slipfork` binary built from this workspace.
 *
 * Nothing here is mocked: every call below spawns `target/debug/slipfork`, with an isolated
 * allowlisted child environment and `XDG_*` directories under `/var/tmp`. The live fork/drop
 * journey runs only when `SLIPFORK_TEST_ROOT` points at a Btrfs mount, and the off-lab journey
 * asserts the CLI's own refusal remedies verbatim.
 */
import { spawnSync } from "node:child_process";
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
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";
import { Effect } from "effect";

import plugin from "../src/index.ts";
import { CliSlipforkGateway, decodeCliFailure } from "../src/cli.ts";
import {
  type ShellCreateBefore,
  SlipforkWorktreeCoordinator,
  forkDirectory,
} from "../src/coordinator.ts";

const REPO = fileURLToPath(new URL("../../../", import.meta.url));
const BINARY = process.env.SLIPFORK_BIN ?? join(REPO, "target", "debug", "slipfork");
const BTRFS_MAGIC = 2435016766;
const CHILD_ENV_KEYS = ["PATH", "SYSTEMROOT", "HOME", "XDG_RUNTIME_DIR", "XDG_STATE_HOME"] as const;

const runtimes: string[] = [];
const roots: string[] = [];

afterAll(() => {
  for (const runtime of runtimes) shutdown(runtime);
  for (const root of roots) rmSync(root, { recursive: true, force: true });
}, 60_000);

function binary(): string {
  if (!existsSync(BINARY)) {
    throw new Error(`missing ${BINARY}; next: run \`cargo build -p slipfork\` or set SLIPFORK_BIN`);
  }
  return BINARY;
}

function childEnv(runtime: string, state: string): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = {};
  for (const key of CHILD_ENV_KEYS) env[key] = process.env[key];
  env.XDG_RUNTIME_DIR = runtime;
  env.XDG_STATE_HOME = state;
  return env;
}

/** Reproduce the composition root's allowlisted `process.env` snapshot (`src/index.ts`). */
function pluginEnv(): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = {};
  for (const key of CHILD_ENV_KEYS) env[key] = process.env[key];
  return env;
}

/** The CLI's own `message\nnext:` refusal document, decoded exactly like `src/cli.ts`. */
function refusal(
  args: readonly string[],
  cwd: string,
  env: Record<string, string | undefined>,
): string {
  const raw = spawnSync(binary(), [...args], { cwd, env, encoding: "utf8", timeout: 30_000 });
  expect(raw.status).not.toBe(0);
  const decoded = decodeCliFailure(String(raw.stdout)) ?? decodeCliFailure(String(raw.stderr));
  expect(decoded).toBeDefined();
  expect(decoded).toContain("\nnext: ");
  if (decoded === undefined) throw new Error(`no refusal document for slipfork ${args.join(" ")}`);
  return decoded;
}

interface Lab {
  root: string;
  project: string;
  runtime: string;
  coordinator: SlipforkWorktreeCoordinator;
  env: Record<string, string | undefined>;
}

/** One project + one daemon runtime for the whole journey. */
function lab(root: string): Lab {
  const project = join(root, "demo");
  mkdirSync(project, { recursive: true });
  const runtime = join(root, "runtime");
  mkdirSync(runtime, { recursive: true, mode: 0o700 });
  if (!runtimes.includes(runtime)) runtimes.push(runtime);
  if (!roots.includes(root)) roots.push(root);
  const env = childEnv(runtime, join(root, "state"));
  const coordinator = new SlipforkWorktreeCoordinator(
    project,
    new CliSlipforkGateway({ binary: binary(), env }),
  );
  return { root, project, runtime, coordinator, env };
}

function scratch(): string {
  return mkdtempSync("/var/tmp/slipfork-plugin-");
}

function daemonPids(runtime: string): number[] {
  const pids: number[] = [];
  for (const entry of readdirSync("/proc")) {
    if (!/^[0-9]+$/.test(entry)) continue;
    try {
      const environ = readFileSync(`/proc/${entry}/environ`, "utf8").split("\0");
      if (!environ.includes(`XDG_RUNTIME_DIR=${runtime}`)) continue;
      const argv = readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0");
      if (argv.includes("__daemon")) pids.push(Number(entry));
    } catch {
      // The process vanished between readdir and read: nothing to reap.
    }
  }
  return pids;
}

function sleep(ms: number): void {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function waitFor(check: () => boolean, timeoutMs: number): boolean {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (check()) return true;
    sleep(25);
  }
  return check();
}

/** Ask the daemon to exit on its socket, then SIGTERM anything still bound to this runtime. */
function shutdown(runtime: string): void {
  const socket = join(runtime, "daemon.sock");
  if (existsSync(socket)) {
    try {
      writeFileSync(
        socket,
        `${JSON.stringify({ v: 1, id: "teardown", method: "shutdown", params: {} })}\n`,
      );
    } catch {
      // A closed socket is already gone; the pid sweep below is the backstop.
    }
    waitFor(() => !existsSync(socket), 5_000);
  }
  for (const pid of daemonPids(runtime)) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {
      // Already exited.
    }
  }
  waitFor(() => daemonPids(runtime).length === 0, 5_000);
  rmSync(runtime, { recursive: true, force: true });
  const leaked = daemonPids(runtime);
  if (leaked.length > 0) {
    throw new Error(
      `slipfork __daemon still alive for ${runtime} (pids ${leaked.join(",")}); next: kill -TERM them`,
    );
  }
}

// ---------------------------------------------------------------------------------------------
// One real-CLI journey: refusal remedies equal the binary's own document, and routing is exact.
// ---------------------------------------------------------------------------------------------

test("real CLI journey: refusals, listing, shell exports, and session reaping", async () => {
  const probe = lab(scratch());

  // The refusal message must equal the CLI's own `message\nnext:` document from this binary.
  const decoded = refusal(["fork", "feat", "--json"], probe.project, probe.env);
  const forkError = await probe.coordinator
    .createFork({
      sourceDirectory: probe.project,
      directory: join(probe.project, ".forks", "feat"),
    })
    .then(() => undefined, (reason: Error) => reason);
  expect(forkError?.message).toBe(`slipfork fork 'feat' failed: ${decoded}`);
  expect(existsSync(join(probe.project, ".forks"))).toBe(false);

  await expect(probe.coordinator.listForks(probe.project)).resolves.toEqual([
    { directory: probe.project, type: "root" },
  ]);

  const decodedDrop = refusal(["drop", "feat", "--json", "--force"], probe.project, probe.env);
  const dropError = await probe.coordinator
    .removeFork({ directory: forkDirectory(probe.project, "feat"), force: true })
    .then(() => undefined, (reason: Error) => reason);
  expect(dropError?.message).toBe(`slipfork drop 'feat' failed: ${decodedDrop}`);

  // A created fork (recorded by the coordinator) exports its world and URL verbatim.
  const checkout = probe.coordinator.adoptFork({
    fork: "feat",
    world: "feat--demo",
    project: forkDirectory(probe.project, "feat"),
    url: "http://feat--demo.localhost",
  });
  mkdirSync(join(checkout, "src"), { recursive: true });
  const event: ShellCreateBefore = {
    command: "pytest -q",
    cwd: join(checkout, "src"),
    timeout: 30_000,
    shell: "/bin/bash",
    env: {},
  };
  expect(await probe.coordinator.routeShell(event)).toEqual({
    kind: "routed",
    fork: "feat",
    world: "feat--demo",
  });
  expect(event.env).toEqual({
    SLIPFORK_WORLD: "feat--demo",
    SLIPFORK_URL: "http://feat--demo.localhost",
  });
  expect(event.command).toBe("pytest -q");
  expect(event.shell).toBe("/bin/bash");

  const outside = { ...event, cwd: join(probe.project, "src"), env: {} };
  expect(await probe.coordinator.routeShell(outside)).toEqual({
    kind: "untouched",
    reason: "not-a-fork",
  });
  expect(outside.env).toEqual({});

  const sessionLine = probe.coordinator.describeSession(join(checkout, "src"));
  expect(sessionLine).toContain("http://feat--demo.localhost");
  expect(sessionLine).toContain("SLIPFORK_WORLD + SLIPFORK_URL");

  // Idle reaping: an observed checkout is never dropped; an owned one is attempted once.
  const observed = forkDirectory(probe.project, "observed");
  mkdirSync(observed, { recursive: true });
  probe.coordinator.bindSession("ses_observed", observed);
  await expect(probe.coordinator.reapSession("ses_observed")).resolves.toEqual([]);
  probe.coordinator.bindSession("ses_owned", checkout);
  const warnings = await probe.coordinator.reapSession("ses_owned");
  expect(warnings).toHaveLength(1);
  expect(warnings[0]).toContain("cannot drop fork 'feat'");
});

// ---------------------------------------------------------------------------------------------
// One composition-root journey: the real factory registers every hook and drives each one.
// ---------------------------------------------------------------------------------------------

interface Harness {
  ctx: Parameters<typeof plugin.effect>[0];
  worktrees: {
    id: string;
    create: (input: { sourceDirectory: string; directory: string; branch?: string }) => Effect.Effect<{ directory: string }, unknown>;
    remove: (input: { directory: string; force: boolean }) => Effect.Effect<void, unknown>;
    list: (source: string) => Effect.Effect<readonly { directory: string; type: "root" | "worktree" }[], unknown>;
  }[];
  shell: ((event: ShellCreateBefore) => Effect.Effect<void>)[];
  context: ((event: { sessionID: string; system: { type: "text"; text: string }[] }) => Effect.Effect<void>)[];
  idle: ((event: { sessionID: string; background: readonly unknown[] }) => Effect.Effect<void>)[];
}

function harness(project: string, sessionDirectory: string): Harness {
  const harness: Harness = { ctx: undefined as never, worktrees: [], shell: [], context: [], idle: [] };
  harness.ctx = {
    options: { binary: binary() },
    location: {
      directory: project,
      project: { id: { global: project }, directory: project, canonical: project },
    },
    worktree: {
      transform: (callback: (editor: { add(definition: Harness["worktrees"][number]): void }) => void) =>
        Effect.sync(() => {
          callback({ add: (definition) => harness.worktrees.push(definition) });
          return { dispose: Effect.void };
        }),
    },
    shell: {
      hook: (_name: string, callback: Harness["shell"][number]) =>
        Effect.sync(() => {
          harness.shell.push(callback);
          return { dispose: Effect.void };
        }),
    },
    session: {
      get: () => Effect.succeed({ location: { directory: sessionDirectory } }),
      hook: (name: string, callback: never) =>
        Effect.sync(() => {
          if (name === "context") harness.context.push(callback);
          else harness.idle.push(callback);
          return { dispose: Effect.void };
        }),
    },
  } as unknown as Parameters<typeof plugin.effect>[0];
  return harness;
}

test("composition root: Plugin.define registers every hook and this binary documents world", async () => {
  const help = spawnSync(binary(), ["fork", "--help"], { encoding: "utf8" });
  expect(String(help.stdout)).toContain('{"fork","world","project","url","elapsed_ms","snapshot_ms"}');

  const probe = lab(scratch());
  const hooks = harness(probe.project, probe.project);
  await Effect.runPromise(Effect.scoped(plugin.effect(hooks.ctx)));

  expect(plugin.id).toBe("slipfork");
  expect(hooks.worktrees).toHaveLength(1);
  expect(hooks.shell).toHaveLength(1);
  expect(hooks.context).toHaveLength(1);
  expect(hooks.idle).toHaveLength(1);

  const definition = hooks.worktrees[0]!;
  expect(definition.id).toBe("slipfork");

  // Both refusals equal this binary's own `message\nnext:` document from this checkout.
  const decodedFork = refusal(["fork", "feat", "--json"], probe.project, pluginEnv());

  const created = await Effect.runPromise(
    definition.create({
      sourceDirectory: probe.project,
      directory: join(probe.project, ".forks", "feat"),
    }),
  ).then(
    () => "created",
    (error: Error) => error.message,
  );
  expect(created).toBe(`slipfork fork 'feat' failed: ${decodedFork}`);

  const listed = await Effect.runPromise(definition.list(probe.project));
  expect(listed).toEqual([{ directory: probe.project, type: "root" }]);

  const decodedDrop = refusal(["drop", "feat", "--json", "--force"], probe.project, pluginEnv());

  const removed = await Effect.runPromise(
    definition.remove({ directory: forkDirectory(probe.project, "feat"), force: true }),
  ).then(
    () => "removed",
    (error: Error) => error.message,
  );
  expect(removed).toBe(`slipfork drop 'feat' failed: ${decodedDrop}`);

  const shellEvent: ShellCreateBefore = {
    command: "true",
    cwd: probe.project,
    timeout: 1_000,
    shell: "/bin/sh",
    env: {},
  };
  await Effect.runPromise(hooks.shell[0]!(shellEvent));
  expect(shellEvent.env).toEqual({});

  const contextEvent = { sessionID: "ses_1", system: [] as { type: "text"; text: string }[] };
  await Effect.runPromise(hooks.context[0]!(contextEvent));
  expect(contextEvent.system).toEqual([]);

  // A finishing shell/subagent keeps the fork: the idle hook returns without reaping.
  await Effect.runPromise(hooks.idle[0]!({ sessionID: "ses_1", background: [{ kind: "shell" }] }));
  await Effect.runPromise(hooks.idle[0]!({ sessionID: "ses_1", background: [] }));
});

// ---------------------------------------------------------------------------------------------
// Live Btrfs journey (opt-in).
// ---------------------------------------------------------------------------------------------

function liveRoot(): string {
  const root = process.env.SLIPFORK_TEST_ROOT;
  if (root === undefined || root.length === 0) {
    throw new Error(
      "live fork journey needs SLIPFORK_TEST_ROOT on a Btrfs mount; next: mount a loop-backed Btrfs file and set SLIPFORK_TEST_ROOT=/mnt/slipfork-live",
    );
  }
  if (statfsSync(root).type !== BTRFS_MAGIC) {
    throw new Error(
      `${root} is not Btrfs; next: mount a loop-backed Btrfs file and set SLIPFORK_TEST_ROOT to it`,
    );
  }
  return root;
}

test.skipIf(process.env.SLIPFORK_TEST_ROOT === undefined)(
  "live fork boots its own world and drop reaps it",
  async () => {
    const { project, coordinator } = lab(liveRoot());
    try {
      const created = await coordinator.createFork({
        sourceDirectory: project,
        directory: join(project, ".forks", "feat"),
      });
      expect(created.directory).toBe(forkDirectory(project, "feat"));
      const listed = await coordinator.listForks(project);
      expect(listed.some((entry) => entry.directory === created.directory)).toBe(true);
      await coordinator.removeFork({ directory: created.directory, force: true });
      const after = await coordinator.listForks(project);
      expect(after.some((entry) => entry.directory === created.directory)).toBe(false);
    } finally {
      await coordinator
        .removeFork({ directory: forkDirectory(project, "feat"), force: true })
        .catch(() => undefined);
    }
  },
  180_000,
);
