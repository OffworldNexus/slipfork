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
import { SLIPFORK, parseForkedWorld, parseWorldList } from "../src/cli.ts";

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
    expect(contract.subcommands[SLIPFORK.command.list].requiredTokens).toContain(SLIPFORK.flag.json);

    expect(contract.jsonFields.fork).toEqual(
      expect.arrayContaining([SLIPFORK.field.world, SLIPFORK.field.origin]),
    );
    expect(contract.jsonFields.list).toEqual(
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

    const rows = parseWorldList(
      JSON.stringify([
        {
          [SLIPFORK.field.world]: "acme:feat",
          [SLIPFORK.field.origin]: "http://feat.acme.localhost:3000",
          [SLIPFORK.field.worktree]: "/repo/.worktrees/feat",
        },
      ]),
    );
    expect(rows).toEqual([
      { world: "acme:feat", origin: "http://feat.acme.localhost:3000", worktree: "/repo/.worktrees/feat" },
    ]);
    expect(() => parseWorldList("{}")).toThrow("did not print a JSON array");
    expect(() => parseWorldList(JSON.stringify([{ world: "acme:feat" }]))).toThrow(
      "missing the 'origin' string field",
    );
  });
});
