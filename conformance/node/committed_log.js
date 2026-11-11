#!/usr/bin/env node
"use strict";
/**
 * The Node client's committed log round (spec/c-abi.md, version 2; ADR 0070):
 * `Client.committedLog` and `CommittedLog` beside a cluster member that
 * `conformance/node/run.sh` started with `cavalry run`, one of three on the loopback, as
 * `conformance/python/committed_log.py` runs it through `clients/python`.
 *
 *     committed_log.js <the member's driver directory> <the member's id>
 *
 * A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
 * bytes in a frame of 88 at position 0, so the log's next position is 96. The round reads
 * that entry through the handler, with its position, its header and the handle's reads;
 * makes each call the ABI refuses from inside the log's own handler; checks where a
 * handler's exception goes and that its entry is not delivered again; opens the log again at
 * 96, where nothing is committed yet; closes twice; and closes a log after its client. Every
 * wait is bounded at 30 s. It exits 1 with what it found when a check fails. Node's standard
 * library only (ADR 0003).
 */

const path = require("path");

const cavalry = require(path.join(__dirname, "..", "..", "clients", "node"));
const { addon, constants } = cavalry;

const WAIT_MS = 30000;
const ENTRY_FRAME = 88; // the term's first entry: 32 bytes of header and 56 of message
const NEXT = 96; // the frame, rounded up to a multiple of 32

function fail(what) {
  process.stderr.write(`conformance/node: committed log: ${what}\n`);
  process.exit(1);
}

function say(what) {
  process.stdout.write(`conformance/node: committed log: ${what}\n`);
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** The member's committed log at `position`, asked for until the member has one. */
function openLog(client, position) {
  const until = Date.now() + WAIT_MS;
  for (;;) {
    const log = client.committedLog(position);
    if (log !== null) return log;
    if (Date.now() > until) fail(`the driver at ${client.dir} hosted no member with a log within ${WAIT_MS / 1000} s`);
    sleep(50);
  }
}

function pollUntil(log, handler, done, what) {
  const until = Date.now() + WAIT_MS;
  while (!done()) {
    if (log.poll(handler, 16) < 0) fail(`${what}: the poll stopped on fault ${JSON.stringify(log.fault())}`);
    if (Date.now() > until) {
      fail(`${what}: not within ${WAIT_MS / 1000} s; position ${log.position()}, commit position ` +
        `${log.commitPosition()}, ended ${log.isEnded}`);
    }
  }
}

function same(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function main() {
  if (process.argv.length !== 4) fail("usage: committed_log.js <the member's driver directory> <the member's id>");
  const memberDir = process.argv[2];
  const memberId = Number(process.argv[3]);
  const client = new cavalry.Client(memberDir);
  const log = openLog(client, 0);
  if (log.memberId !== memberId || log.recordingId !== 0) {
    fail(`the log is member ${log.memberId}'s recording ${log.recordingId}, not member ${memberId}'s recording 0`);
  }
  if (log.position() !== 0 || log.commitPosition() !== 0) {
    fail(`a log opened at 0 began at ${log.position()} with bound ${log.commitPosition()}`);
  }

  // The entry, with what each call on the log answers from inside its own handler.
  const entries = [];
  const refusals = [];
  const onEntry = (body, header, position) => {
    entries.push([position, body.length, header.frameLength, header.flags, header.type]);
    if (refusals.length > 0) return;
    refusals.push(["poll", log.poll(onEntry, 1), addon.cavalry_errcode()]);
    refusals.push(["position", log.position(), addon.cavalry_errcode()]);
    refusals.push(["commitPosition", log.commitPosition(), addon.cavalry_errcode()]);
    refusals.push(["isEnded", log.isEnded, addon.cavalry_errcode()]);
    refusals.push(["failedAt", log.failedAt(), addon.cavalry_errcode()]);
    refusals.push(["withdrawnAt", log.withdrawnAt(), addon.cavalry_errcode()]);
    try {
      log.close();
      refusals.push(["close", 0, 0]);
    } catch (thrown) {
      refusals.push(["close", -1, thrown.code]);
    }
  };
  pollUntil(log, onEntry, () => entries.length > 0, "the term's first entry");
  const expectedEntry = [[0, ENTRY_FRAME - 32, ENTRY_FRAME, constants.CAVALRY_FLAGS_UNFRAGMENTED,
    constants.CAVALRY_FRAME_TYPE_DATA]];
  if (!same(entries, expectedEntry)) {
    fail(`expected one entry of ${ENTRY_FRAME - 32} bytes at position 0 in an unfragmented data frame of ` +
      `${ENTRY_FRAME}, got ${JSON.stringify(entries)}`);
  }
  const polling = constants.CAVALRY_ERR_POLLING;
  const expected = [["poll", -1, polling], ["position", -1, polling], ["commitPosition", -1, polling],
    ["isEnded", false, polling], ["failedAt", null, polling], ["withdrawnAt", null, polling],
    ["close", -1, polling]];
  if (!same(refusals, expected)) {
    fail(`the calls from inside the log's own handler answered ${JSON.stringify(refusals)}, not ` +
      `${JSON.stringify(expected)}`);
  }
  if (log.position() !== NEXT || log.commitPosition() !== NEXT) {
    fail(`after the entry the log is at ${log.position()} with bound ${log.commitPosition()}, not ${NEXT} and ${NEXT}`);
  }
  const fault = log.fault();
  if (log.isEnded || log.failedAt() !== null || log.withdrawnAt() !== null || fault.code !== 0 ||
      fault.sessionId !== 0 || fault.position !== cavalry.NULL_POSITION) {
    fail(`a live log read ended ${log.isEnded}, failed at ${log.failedAt()}, withdrawn at ` +
      `${log.withdrawnAt()}, fault ${JSON.stringify(fault)}`);
  }
  for (let i = 0; i < 50; i++) {
    if (log.poll(onEntry, 16) !== 0) fail("an entry arrived with nothing more committed");
  }
  if (entries.length !== 1) fail(`the refused poll, or a later one, delivered more: ${JSON.stringify(entries)}`);
  say(`member ${log.memberId}'s entry at position 0 read once, next position ${log.position()}, commit position ` +
    `${log.commitPosition()}; seven calls from inside the handler refused with CAVALRY_ERR_POLLING`);
  log.close();
  log.close(); // a second close does nothing

  // A handler that throws: the entry was delivered, the exception is thrown after the ABI
  // has returned, and the log's position is past the entry, so it is not delivered again.
  const throwing = openLog(client, 0);
  const seen = [];
  const throws = (body, header, position) => {
    seen.push(position);
    throw new RangeError("the handler's own");
  };
  const until = Date.now() + WAIT_MS;
  let thrown = null;
  while (thrown === null) {
    try {
      if (throwing.poll(throws, 16) < 0) fail(`the second log's poll stopped on fault ${JSON.stringify(throwing.fault())}`);
    } catch (error) {
      if (!(error instanceof cavalry.HandlerError)) throw error;
      thrown = error;
    }
    if (thrown === null && Date.now() > until) fail("the second log read no entry");
  }
  if (!same(seen, [0]) || !(thrown.cause instanceof RangeError) || thrown.raised !== 1 || thrown.undelivered !== 0 ||
      throwing.position() !== NEXT) {
    fail(`a throwing handler saw ${JSON.stringify(seen)}, threw ${thrown} from ${thrown.cause}, raised ` +
      `${thrown.raised}, undelivered ${thrown.undelivered}, and left the log at ${throwing.position()}`);
  }
  for (let i = 0; i < 50; i++) {
    if (throwing.poll(throws, 16) !== 0) fail("the entry whose handler threw was delivered again");
  }
  throwing.close();

  // From the next position nothing is committed yet: no entry, and no entry before it.
  const rest = openLog(client, NEXT);
  if (rest.position() !== NEXT) fail(`a log opened at ${NEXT} began at ${rest.position()}`);
  for (let i = 0; i < 100; i++) {
    if (rest.poll(onEntry, 16) !== 0) fail(`a log opened at ${NEXT} delivered an entry: ${JSON.stringify(entries)}`);
  }
  // A log that outlives its client: its poll stops on the closed fault, and its close frees
  // it with no client to stop the replay through.
  client.close();
  if (rest.poll(onEntry, 16) !== -1 || rest.fault().code !== constants.CAVALRY_ERR_FAULT_CLOSED) {
    fail(`a poll after the client's close did not stop on the closed fault: ${JSON.stringify(rest.fault())}`);
  }
  rest.close();
  say(`a throwing handler's entry is not delivered again; nothing at position ${NEXT}; a log closed after its client`);
}

try {
  main();
} catch (thrown) {
  if (thrown instanceof cavalry.CavalryError) fail(`${thrown.fn} failed with error ${thrown.code}: ${thrown.abiMessage}`);
  if (thrown instanceof cavalry.HandlerError) fail(`the handler threw: ${thrown.cause}`);
  throw thrown;
}
