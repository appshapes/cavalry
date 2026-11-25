#!/usr/bin/env node
"use strict";
/* The Node client's closed round (spec/c-abi.md, "cluster service"; ADR 0155): a
 * `ClusterService` whose driver stops. A poll with a client its driver closed is one answer,
 * -1 with CAVALRY_ERR_CLOSED as the thread's error and as `fault()`'s code, at the first poll
 * that fails and at every poll after it, with the client's reason
 * CAVALRY_CLOSED_DRIVER_CLOSED; and the service's close is then no error. The driver hosts no
 * member, so the service holds no log and its poll has no command to send: the case in which
 * a poll of a closed client took nothing and returned 0 for as long as it was called.
 * `conformance/node/run.sh` runs it last, against the driver its quickstart ran on.
 *
 *     closed.js <the driver's process id>
 *
 * The client attaches to the driver at CAVALRY_DIR, and the program stops that driver itself,
 * with SIGTERM to the process id it was given. A failed check prints what failed and exits 1;
 * the wait is bounded at 60 s. Node's standard library only (ADR 0003).
 */

const path = require("path");

const cavalry = require(path.join(__dirname, "..", "..", "clients", "node"));
const { addon, constants } = cavalry;

const WAIT_MS = 60000;
const POLLS_AFTER = 3;
const CLOSED = constants.CAVALRY_ERR_CLOSED;

function fail(what) {
  process.stderr.write(`conformance/node: closed round: ${what}\n`);
  process.exit(1);
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function main() {
  const pid = Number(process.argv[2]);
  if (process.argv.length !== 3 || !Number.isInteger(pid) || pid <= 1) fail("usage: closed.js <the driver's process id>");
  const handed = [];
  const onEntry = (body, entry) => {
    handed.push(entry.type);
  };

  const client = new cavalry.Client();
  const service = client.clusterService();

  // 1. Beside a driver that runs and hosts no member, a poll takes nothing.
  let taken = service.poll(onEntry, 10);
  if (taken !== 0 || client.isClosed) {
    fail(`a poll beside a driver with no member returned ${taken}, and the client reads closed ${client.closed()}; ` +
      `expected 0 beside a driver that runs`);
  }

  // 2. The driver stops. Until the client has seen it, a poll takes nothing; the first poll
  // that fails is the close, and nothing else.
  process.kill(pid, "SIGTERM");
  const until = Date.now() + WAIT_MS;
  let polls = 0;
  let errcode;
  for (;;) {
    taken = service.poll(onEntry, 10);
    errcode = addon.cavalry_errcode();
    polls += 1;
    if (taken === -1) break;
    if (taken !== 0) fail(`a poll beside a driver with no member returned ${taken}; expected 0, or -1 after the stop`);
    if (Date.now() > until) {
      fail(`no poll failed within ${WAIT_MS / 1000} s of the driver's stop; the client reads closed ${client.closed()}`);
    }
    sleep(1);
  }
  let code = service.fault().code;
  if (code !== CLOSED || errcode !== CLOSED) {
    fail(`the first poll that failed after the driver's stop stopped on code ${code}, with the thread's error ` +
      `${errcode}; expected CAVALRY_ERR_CLOSED, ${CLOSED}, as both`);
  }
  if (!client.isClosed || client.closed() !== constants.CAVALRY_CLOSED_DRIVER_CLOSED) {
    fail(`the poll said closed, and the client reads closed ${client.closed()}; expected ` +
      `CAVALRY_CLOSED_DRIVER_CLOSED, ${constants.CAVALRY_CLOSED_DRIVER_CLOSED}`);
  }

  // 3. Every poll after it is the same answer: a loop that looks at what its poll returned
  // ends.
  for (let after = 1; after <= POLLS_AFTER; after++) {
    // The thread's error to another code first: a success clears nothing, so without this the
    // code read below could be the first failed poll's.
    if (addon.cavalry_committed_log_entry_position(null) !== -1 || addon.cavalry_errcode() !== constants.CAVALRY_ERR_NULL) {
      fail(`the position of a null header did not answer -1 with CAVALRY_ERR_NULL: ${addon.cavalry_errcode()}`);
    }
    taken = service.poll(onEntry, 10);
    errcode = addon.cavalry_errcode();
    code = service.fault().code;
    if (taken !== -1 || code !== CLOSED || errcode !== CLOSED) {
      fail(`poll ${after} after the first that failed returned ${taken} with code ${code} and the thread's error ` +
        `${errcode}; expected -1 with CAVALRY_ERR_CLOSED as both`);
    }
  }
  if (handed.length !== 0) fail(`the handler was called beside a driver with no member: ${JSON.stringify(handed)}`);

  // 4. The service closes with no error, and its client after it: a driver that is gone
  // cannot be told.
  service.close();
  client.close();
  process.stdout.write(`conformance/node: closed round: a service beside a driver with no member took nothing; ` +
    `after the driver's stop, poll ${polls} and the ${POLLS_AFTER} after it returned -1 with CAVALRY_ERR_CLOSED, the ` +
    `client's reason is CAVALRY_CLOSED_DRIVER_CLOSED, and the service closed with no error\n`);
  process.stdout.write("conformance/node: closed round ok\n");
}

try {
  main();
} catch (thrown) {
  if (thrown instanceof cavalry.CavalryError) fail(`${thrown.fn} failed with error ${thrown.code}: ${thrown.abiMessage}`);
  throw thrown;
}
