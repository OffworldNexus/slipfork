/**
 * Pure contract tests: JSON documents copied from the shipped `slipfork --json` output, the
 * binary lookup against a real executable script on disk, and the coordinator's path rules.
 */
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";

import {
  decodeCliFailure,
  flattenForkRows,
  parseForkedWorld,
  parseStatusWorlds,
  resolveBinary,
} from "../src/cli.ts";
import { forkDirectory, forkFromCheckout } from "../src/coordinator.ts";

function scratch(prefix: string): string {
  return mkdtempSync(join("/var/tmp", `${prefix}-`));
}

describe("slipfork CLI documents", () => {
  test("fork success names the world and its URL", () => {
    const forked = parseForkedWorld(
      JSON.stringify({
        fork: "feat-auth",
        world: "feat-auth--demo",
        project: "/projects/demo/.forks/feat-auth",
        url: "http://feat-auth--demo.localhost",
        elapsed_ms: 42,
      }),
      "feat-auth",
    );
    expect(forked.world).toBe("feat-auth--demo");
    expect(forked.url).toBe("http://feat-auth--demo.localhost");
  });

  test("failure keeps the CLI's message and next remedy verbatim", () => {
    const failure = JSON.stringify({
      error: {
        code: "not-btrfs",
        message: "`/projects/demo` is not a Btrfs subvolume",
        next: "move the project onto a btrfs mount, then retry `slipfork fork`",
      },
    });
    expect(decodeCliFailure(failure)).toBe(
      "`/projects/demo` is not a Btrfs subvolume\n" +
        "next: move the project onto a btrfs mount, then retry `slipfork fork`",
    );
  });

  test("status nests forks under their parent world", () => {
    const rows = parseStatusWorlds(
      JSON.stringify({
        worlds: [
          {
            project: "/projects/demo",
            path: "/projects/demo",
            world: "demo",
            url: "http://demo.localhost",
            forks: [
              {
                project: "/projects/demo/.forks/feat-auth",
                path: "/projects/demo/.forks/feat-auth",
                world: "feat-auth--demo",
                fork: "feat-auth",
                url: "http://feat-auth--demo.localhost",
              },
            ],
          },
        ],
      }),
    );
    const flat = flattenForkRows(rows);
    expect(flat.map((row) => row.world)).toEqual(["demo", "feat-auth--demo"]);
    expect(flat[1]?.fork).toBe("feat-auth");
  });
});

describe("binary lookup", () => {
  test("explicit option wins, then PATH finds a real executable", () => {
    const dir = scratch("slipfork-bin");
    try {
      const script = join(dir, "slipfork");
      writeFileSync(script, "#!/bin/sh\nexit 0\n");
      chmodSync(script, 0o755);
      const env = { PATH: `${dir}:/usr/bin` };
      expect(resolveBinary("/opt/slipfork", env)).toBe("/opt/slipfork");
      expect(resolveBinary(undefined, env)).toBe(script);
      expect(resolveBinary(undefined, { PATH: join(dir, "missing") })).toBeUndefined();
      expect(resolveBinary(undefined, {})).toBeUndefined();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("checkout paths", () => {
  test("fork checkouts live under <project>/.forks/<fork>", () => {
    const root = "/projects/demo";
    expect(forkDirectory(root, "feat-auth")).toBe("/projects/demo/.forks/feat-auth");
    expect(forkFromCheckout(root, "/projects/demo/.forks/feat-auth/src")).toBe("feat-auth");
    expect(forkFromCheckout(root, "/projects/demo/src")).toBeUndefined();
  });
});
