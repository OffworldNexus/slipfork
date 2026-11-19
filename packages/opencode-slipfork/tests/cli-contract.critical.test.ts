/**
 * Protected CLI-contract test (`M2.18`).
 *
 * `src/cli.ts` is the only place that spells a `slipfork` subcommand, flag, or JSON field. This
 * test proves that declaration against `tests/fixtures/cli-contract.json`, which records what the
 * real binary printed, and against the binary itself — so a CLI rename fails the suite instead of
 * drifting silently.
 */
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { SLIPFORK, decodeCliFailure, parseForkedWorld, parseStatusWorlds } from "../src/cli.ts";

const REPO_ROOT = join(import.meta.dir, "..", "..", "..");

interface SubcommandContract {
  argv: string[];
  requiredTokens: string[];
}

interface CliContract {
  recordedFrom: string;
  topLevel: SubcommandContract;
  subcommands: Record<string, SubcommandContract>;
  jsonFields: Record<string, string[]>;
}

const contract: CliContract = JSON.parse(
  readFileSync(join(import.meta.dir, "fixtures", "cli-contract.json"), "utf8"),
);

/** The compiled binary, built once through the workspace's own Cargo target. */
function slipforkBinary(): string {
  const binary = join(REPO_ROOT, "target", "debug", "slipfork");
  if (!existsSync(binary)) {
    execFileSync("cargo", ["build", "-p", "slipfork", "--bins"], {
      cwd: REPO_ROOT,
      stdio: "inherit",
    });
  }
  if (!existsSync(binary)) {
    throw new Error(`expected the slipfork binary at ${binary}; run \`cargo build -p slipfork\``);
  }
  return binary;
}

function help(binary: string, argv: string[]): string {
  return execFileSync(binary, argv, { encoding: "utf8" });
}

describe("slipfork CLI contract fixture (M2.18)", () => {
  test("every subcommand the plugin drives is recorded in the fixture", () => {
    expect(Object.keys(contract.subcommands).sort()).toEqual(
      Object.values(SLIPFORK.command).sort(),
    );
    for (const command of Object.values(SLIPFORK.command)) {
      const recorded = contract.subcommands[command];
      expect(recorded).toBeDefined();
      expect(recorded.argv[0]).toBe(command);
    }
  });

  test("the declared flags and JSON fields are the recorded ones", () => {
    const exec = contract.subcommands[SLIPFORK.command.exec];
    expect(exec.requiredTokens).toContain(`${SLIPFORK.flag.world} <WORLD>`);
    expect(exec.requiredTokens).toContain(`${SLIPFORK.flag.cwd} <CWD>`);

    const fork = contract.subcommands[SLIPFORK.command.fork];
    expect(fork.requiredTokens).toContain(`${SLIPFORK.flag.base} <BASE>`);
    expect(fork.requiredTokens).toContain(SLIPFORK.flag.json);
    expect(contract.subcommands[SLIPFORK.command.status].requiredTokens).toContain(
      SLIPFORK.flag.json,
    );

    expect(contract.jsonFields.fork).toEqual(
      expect.arrayContaining([SLIPFORK.field.world, SLIPFORK.field.origin]),
    );
    // The status document is the envelope; every world row carries the fields the plugin reads.
    expect(contract.jsonFields[SLIPFORK.command.status]).toEqual(
      expect.arrayContaining([
        SLIPFORK.field.ok,
        SLIPFORK.field.ingressPort,
        SLIPFORK.field.worlds,
      ]),
    );
    expect(contract.jsonFields.statusWorld).toEqual(
      expect.arrayContaining([
        SLIPFORK.field.world,
        SLIPFORK.field.origin,
        SLIPFORK.field.worktree,
      ]),
    );
  });

  test("the real binary still prints every recorded token", () => {
    const binary = slipforkBinary();
    for (const recorded of [contract.topLevel, ...Object.values(contract.subcommands)]) {
      const output = help(binary, recorded.argv);
      for (const token of recorded.requiredTokens) {
        expect(output).toContain(token);
      }
    }
  });

  test("JSON payloads are decoded from the recorded field names and fail closed otherwise", () => {
    const forked = parseForkedWorld(
      JSON.stringify({ [SLIPFORK.field.world]: "acme:feat", [SLIPFORK.field.origin]: "http://feat.acme.localhost:3000" }),
      "feat",
    );
    expect(forked).toEqual({ world: "acme:feat", origin: "http://feat.acme.localhost:3000" });

    expect(() => parseForkedWorld("{}", "feat")).toThrow("missing the 'world' string field");
    expect(() =>
      parseForkedWorld(
        JSON.stringify({ world: "acme:other", origin: "http://other.acme.localhost:3000" }),
        "feat",
      ),
    ).toThrow("reported world 'acme:other' for fork 'feat'");
    expect(() =>
      parseForkedWorld(JSON.stringify({ world: "feat", origin: "file:///tmp/x" }), "feat"),
    ).toThrow("non-HTTP origin URL");

    const world = (extra: Record<string, unknown>): Record<string, unknown> => ({
      [SLIPFORK.field.world]: "acme:feat",
      [SLIPFORK.field.origin]: "http://feat.acme.localhost:3000",
      [SLIPFORK.field.worktree]: "/repo/.worktrees/feat",
      ...extra,
    });
    const status = (worlds: unknown[]): string =>
      JSON.stringify({ [SLIPFORK.field.ok]: true, [SLIPFORK.field.worlds]: worlds });

    const rows = parseStatusWorlds(status([world({})]));
    expect(rows).toEqual([
      { world: "acme:feat", origin: "http://feat.acme.localhost:3000", worktree: "/repo/.worktrees/feat" },
    ]);

    // A `{"ok":false}` document is never an empty fleet: the error and `next` remedy must surface.
    const refusal = JSON.stringify({
      [SLIPFORK.field.ok]: false,
      code: "error",
      [SLIPFORK.field.error]: "slipforkd is not running",
      [SLIPFORK.field.next]: "slipfork status",
    });
    expect(() => parseStatusWorlds(refusal)).toThrow("slipforkd is not running");
    expect(() => parseStatusWorlds(refusal)).toThrow("next: slipfork status");
    expect(decodeCliFailure(refusal)).toBe(
      "slipforkd is not running\n  next: slipfork status",
    );
    expect(decodeCliFailure("not json")).toBeUndefined();
    expect(decodeCliFailure(JSON.stringify({ [SLIPFORK.field.ok]: true }))).toBeUndefined();

    expect(() => parseStatusWorlds(JSON.stringify({ [SLIPFORK.field.ok]: true }))).toThrow(
      "did not print a 'worlds' array",
    );
    expect(() => parseStatusWorlds(JSON.stringify([]))).toThrow("did not print a JSON object");
    expect(() => parseStatusWorlds(status([{ [SLIPFORK.field.world]: "acme:feat" }]))).toThrow(
      "missing the 'origin' string field",
    );
    expect(() => parseStatusWorlds(status([world({ [SLIPFORK.field.origin]: "file:///x" })]))).toThrow(
      "non-HTTP origin URL",
    );
  });
});
