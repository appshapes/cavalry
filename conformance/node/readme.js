#!/usr/bin/env node
"use strict";
/**
 * Runs the example of clients/node/README.md against the runner's driver; `run.sh` calls it
 * after quickstart.js, so the README's program is a test (CLAUDE.md, "Clients and developer
 * experience"; .claude/rules/docs.md, "Code in docs"). It takes the program between
 * `node - <<'JS'` and `JS` in the README's "Example" and the line the README promises after
 * "You will see", runs the program with this Node on its standard input from the checkout's
 * root, as the README's shell lines do, for up to 10 s, and exits 1 unless the program exits
 * 0 and prints that line. The README's shell lines around the program, the builds and the
 * driver, are run.sh's own steps. Node's standard library only (ADR 0003).
 */

const { spawnSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..", "..");
const README = path.join("clients", "node", "README.md");
const WAIT_MS = 10000;

function fail(what) {
  process.stderr.write(`conformance/node: ${README}'s example: ${what}\n`);
  process.exit(1);
}

function main() {
  const text = fs.readFileSync(path.join(ROOT, README), "utf8");
  const program = /^node - <<'JS'\n([\s\S]*?)^JS$/m.exec(text);
  const expected = /^You will see `([^`]+)`\.$/m.exec(text);
  if (program === null || expected === null) {
    fail("no program between `node - <<'JS'` and `JS`, or no line \"You will see `...`.\" after it; keep both shapes " +
         "so this test can find them");
  }
  const result = spawnSync(process.execPath, ["-"], { input: program[1], cwd: ROOT, timeout: WAIT_MS, encoding: "utf8" });
  if (result.error && result.error.code === "ETIMEDOUT") {
    fail(`did not finish in ${WAIT_MS / 1000} s; it printed ${JSON.stringify(result.stdout)}`);
  }
  if (result.error) fail(`could not run: ${result.error.message}`);
  if (result.status !== 0) {
    fail(`exited ${result.status}; it printed ${JSON.stringify(result.stdout)} and ${JSON.stringify(result.stderr)}`);
  }
  if (!result.stdout.split("\n").includes(expected[1])) {
    fail(`printed ${JSON.stringify(result.stdout)}, not the line the README says the reader will see, ${JSON.stringify(expected[1])}`);
  }
  process.stdout.write(`conformance/node: ${README}'s example printed \`${expected[1]}\`, as the README says\n`);
  return 0;
}

process.exitCode = main();
