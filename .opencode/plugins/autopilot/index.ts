/**
 * Slipfork harness plugin: composition root.
 *
 *  - Autopilot: keeps the top-level `conductor` running while `plan/AUTOPILOT` is "on",
 *    and halts immediately when open `direction-challenge` or `missing-input` items block work.
 *  - ExpensiveBudget: tracks expensive-tier spend (Gemini 4 Carbon / Opus), writes `plan/BUDGET.md`,
 *    and blocks expensive subagents past `expensive_cap_usd` in `plan/BUDGET.json`.
 *
 * Uses OpenCode V2's plain `{ id, setup }` promise plugin contract so zero `node_modules` are needed.
 */
import { Autopilot, type AutopilotCtx } from "./autopilot.ts";
import { EXPENSIVE_AGENTS, ExpensiveBudget, type BudgetCtx } from "./budget.ts";

const WATCHED = [
  "session.idle",
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
] as const;

interface PluginContext extends AutopilotCtx, BudgetCtx {
  location: {
    directory: string;
    project?: { id?: string; directory?: string };
  };
  permission: {
    hook(
      name: "evaluate",
      cb: (event: {
        action: string;
        resources: readonly string[];
        effect: "allow" | "ask" | "deny";
        message?: string;
      }) => Promise<void> | void,
    ): Promise<{ dispose(): Promise<void> }>;
  };
  event: {
    subscribe(opts: {
      types: readonly string[];
      signal: AbortSignal;
    }): AsyncIterable<{ type: string; data?: { sessionID?: string } }>;
  };
}

export default {
  id: "slipfork.autopilot",
  async setup(ctx: PluginContext) {
    const root = String(ctx.location.project?.directory ?? ctx.location.directory);
    const autopilot = new Autopilot(ctx, root);
    const budget = new ExpensiveBudget(ctx, root, ctx.location.project?.id);
    const controller = new AbortController();

    void budget
      .backfill()
      .then(() => budget.refreshReport(true))
      .catch((error: unknown) => console.error("[slipfork-budget] backfill failed", error));

    const permission = await ctx.permission.hook("evaluate", async (event) => {
      if (event.action !== "subagent") return;
      const target = event.resources.find((r) => EXPENSIVE_AGENTS.has(r));
      if (!target) return;
      try {
        const message = await budget.denyMessage(target);
        if (message) {
          event.effect = "deny";
          event.message = message;
        }
      } catch (error) {
        console.error("[slipfork-budget] evaluate failed", error);
      }
    });

    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ types: WATCHED, signal: controller.signal })) {
          const sessionID = event.data?.sessionID;
          if (!sessionID) continue;
          try {
            await budget.recordSession(sessionID);
            await budget.refreshReport();
            if (event.type === "session.idle") await autopilot.onIdle(sessionID);
          } catch (error) {
            console.error("[slipfork-autopilot] event handler failed", error);
          }
        }
      } catch (error) {
        if (!controller.signal.aborted) console.error("[slipfork-autopilot] event stream failed", error);
      }
    })();

    return async () => {
      controller.abort();
      autopilot.dispose();
      await permission.dispose();
    };
  },
};
