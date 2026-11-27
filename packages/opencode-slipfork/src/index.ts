/**
 * `@slipfork/opencode` composition root.
 *
 * This is the only file in the package that reads `process.env`, and it reads only the six names
 * in `CHILD_ENV_KEYS` (the same allowlist idea as `editors/zed/src/lib.rs`). Everything else
 * receives the binary and environment from here, so the coordinator stays pure and the CLI
 * adapter stays a thin argv/JSON boundary.
 */
import { Plugin } from "@opencode/plugin/effect";
import { Effect } from "effect";

import { CliSlipforkGateway, resolveBinary } from "./cli.ts";
import { SlipforkWorktreeCoordinator } from "./coordinator.ts";

const PLUGIN_ID = "slipfork";

/** The only environment names the child `slipfork` process may inherit. */
const CHILD_ENV_KEYS = [
  "PATH",
  "SYSTEMROOT",
  "HOME",
  "XDG_RUNTIME_DIR",
  "XDG_STATE_HOME",
  "XDG_DATA_HOME",
] as const;

/** An allowlisted snapshot: doctor tokens and every other ambient variable stay behind. */
function childEnv(): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = {};
  for (const key of CHILD_ENV_KEYS) env[key] = process.env[key];
  return env;
}

function warn(message: string): Effect.Effect<void> {
  return Effect.sync(() => console.warn(`slipfork: ${message}`));
}

/** Run one cleanup/notification step; a failure is a warning, never a broken plugin hook. */
function absorbing(work: () => Promise<readonly string[] | void>): Effect.Effect<void> {
  return Effect.promise(async () => {
    try {
      const warnings = await work();
      for (const warning of warnings ?? []) console.warn(`slipfork: ${warning}`);
    } catch (error) {
      console.warn(`slipfork: ${(error as Error).message}`);
    }
  });
}

export default Plugin.define({
  id: PLUGIN_ID,
  effect: (ctx) =>
    Effect.gen(function* () {
      const explicit = typeof ctx.options.binary === "string" ? ctx.options.binary : undefined;
      const binary = resolveBinary(explicit, {
        PATH: process.env.PATH,
        SYSTEMROOT: process.env.SYSTEMROOT,
      });
      if (binary === undefined) {
        yield* warn(
          "no `slipfork` binary found; next: install Slipfork (see docs/install.sh in the Slipfork repository) or set `binary` in .opencode/opencode.jsonc",
        );
        return;
      }

      const projectRoot = ctx.location.project.canonical;
      const coordinator = new SlipforkWorktreeCoordinator(
        projectRoot,
        new CliSlipforkGateway({ binary, env: childEnv() }),
      );

      yield* Effect.addFinalizer(() => absorbing(() => coordinator.releaseAll()));

      yield* ctx.worktree.transform((editor) => {
        editor.add({
          id: PLUGIN_ID,
          create: (input) =>
            Effect.tryPromise({
              try: () => coordinator.createFork(input),
              catch: (error) => error,
            }),
          remove: (input) =>
            Effect.tryPromise({
              try: () => coordinator.removeFork(input),
              catch: (error) => error,
            }),
          list: (sourceDirectory) =>
            Effect.tryPromise({
              try: () => coordinator.listForks(sourceDirectory),
              catch: (error) => error,
            }),
        });
      });

      yield* ctx.shell.hook("create.before", (event) =>
        absorbing(async () => {
          const routing = await coordinator.routeShell(event);
          if (routing.kind === "untouched" && routing.reason === "world-unknown") {
            console.warn(
              `slipfork: ${event.cwd} is a fork checkout without a running world; next: run \`slipfork up\` in the project, or \`slipfork fork <name>\` again`,
            );
          }
        }),
      );

      yield* ctx.session.hook("context", (event) =>
        Effect.gen(function* () {
          const info = yield* Effect.catch(
            ctx.session.get({ sessionID: event.sessionID }),
            (error) => {
              console.warn(
                `slipfork: cannot read session ${event.sessionID}; next: reload the OpenCode session; the plugin keeps running`,
              );
              void error;
              return Effect.succeed(undefined);
            },
          );
          if (info === undefined) return;
          coordinator.bindSession(event.sessionID, info.location.directory);
          const line = coordinator.describeSession(info.location.directory);
          if (line !== undefined) event.system.push({ type: "text", text: line });
        }),
      );

      yield* ctx.session.hook("experimental.idle", (event) =>
        event.background.length > 0
          ? Effect.sync(() => {})
          : absorbing(() => coordinator.reapSession(event.sessionID)),
      );
    }),
});
