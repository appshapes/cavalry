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
 * makes each of the eight calls the ABI refuses from inside the log's own handler, each
 * after it has set the thread's error to another code, so that a refusal that answers what
 * a live log answers is told from an answer; opens a log and closes it with no poll
 * between, which leaves its client open (ADR 0078); checks where a handler's exception
 * goes and that its entry is not delivered again; opens the log again at 96, where nothing
 * is committed yet,
 * beside a log at 0 that it reads the entry through meanwhile, so that the log at 96 has had
 * the time a replay's image takes to join; closes twice; and closes a log after its client.
 * Every wait is bounded at 30 s and sleeps a millisecond after a poll that delivered
 * nothing. It exits 1 with what it found when a check fails. Node's standard library only
 * (ADR 0003).
 */

const path = require("path");

const cavalry = require(path.join(__dirname, "..", "..", "clients", "node"));
const { addon, constants } = cavalry;

const WAIT_MS = 30000;
const IDLE_MS = 1; // after a poll that delivered nothing: three drivers share the host
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
    const count = log.poll(handler, 16);
    if (count < 0) fail(`${what}: the poll stopped on fault ${JSON.stringify(log.fault())}`);
    if (Date.now() > until) {
      fail(`${what}: not within ${WAIT_MS / 1000} s; position ${log.position()}, commit position ` +
        `${log.commitPosition()}, ended ${log.isEnded}`);
    }
    if (count === 0) sleep(IDLE_MS);
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
  let inside = false;
  // Sets the thread's error to one no refusal gives, CAVALRY_ERR_NULL, by asking the ABI for
  // the position of a null header. A success clears nothing (spec/c-abi.md, "Error codes"),
  // and a refused isEnded, failedAt or withdrawnAt answers what a live log answers, false or
  // null, so the code alone tells that refusal from an answer: it must have changed from
  // this one to CAVALRY_ERR_POLLING. Without this the code would be CAVALRY_ERR_POLLING
  // already, left by the call before.
  const setAnotherError = () => {
    if (addon.cavalry_committed_log_entry_position(null) !== -1 ||
        addon.cavalry_errcode() !== constants.CAVALRY_ERR_NULL) {
      fail(`the position of a null header did not answer -1 with CAVALRY_ERR_NULL: error ${addon.cavalry_errcode()}`);
    }
  };
  const onEntry = (body, header, position) => {
    entries.push([position, body.length, header.frameLength, header.flags, header.type]);
    if (inside) return;
    inside = true;
    const calls = [["poll", () => log.poll(onEntry, 1)], ["position", () => log.position()],
      ["commitPosition", () => log.commitPosition()], ["isEnded", () => log.isEnded],
      ["failedAt", () => log.failedAt()], ["withdrawnAt", () => log.withdrawnAt()]];
    for (const [name, call] of calls) {
      setAnotherError();
      refusals.push([name, call(), addon.cavalry_errcode()]);
    }
    for (const [name, call] of [["fault", () => log.fault()], ["close", () => log.close()]]) {
      setAnotherError();
      try {
        call();
        refusals.push([name, 0, addon.cavalry_errcode()]);
      } catch (thrown) {
        refusals.push([name, -1, thrown.code]);
      }
    }
  };
  pollUntil(log, onEntry, () => entries.length > 0, "the term's first entry");
  const expectedEntry = [[0, ENTRY_FRAME - 32, ENTRY_FRAME, constants.CAVALRY_FLAGS_UNFRAGMENTED,
    constants.CAVALRY_FRAME_TYPE_DATA]];
  if (!same(entries, expectedEntry)) {
    fail(`expected one entry of ${ENTRY_FRAME - 32} bytes at position 0 in an unfragmented data frame of ` +
      `${ENTRY_FRAME}, got ${JSON.stringify(entries)}. One cause of more than one entry is a new term: a ` +
      `cluster that elected again between its formation and this read holds that term's first entry after ` +
      `this one; the members' output, which run.sh prints, says whether it did`);
  }
  const polling = constants.CAVALRY_ERR_POLLING;
  const expected = [["poll", -1, polling], ["position", -1, polling], ["commitPosition", -1, polling],
    ["isEnded", false, polling], ["failedAt", null, polling], ["withdrawnAt", null, polling],
    ["fault", -1, polling], ["close", -1, polling]];
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
    `${log.commitPosition()}; eight calls from inside the handler refused with CAVALRY_ERR_POLLING, each ` +
    `made with the thread's error at CAVALRY_ERR_NULL`);
  log.close();
  log.close(); // a second close does nothing

  // A log opened and closed with no poll between: its replay's image may not have joined
  // yet, and the client's conductor then reads of an image whose log is gone. The client
  // stays open (ADR 0078; before it, card 241, this closed the client as corrupt in 8 of 60
  // tries), which twenty in a row and a log opened after them show.
  for (let i = 0; i < 20; i++) openLog(client, 0).close();
  if (client.isClosed) fail(`a log opened and closed with no poll between closed its client: ${client.closed()}`);

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
    if (thrown === null) sleep(IDLE_MS);
  }
  if (!same(seen, [0]) || !(thrown.cause instanceof RangeError) || thrown.raised !== 1 || thrown.undelivered !== 0 ||
      throwing.position() !== NEXT) {
    fail(`a throwing handler saw ${JSON.stringify(seen)}, threw ${thrown} from ${thrown.cause}, raised ` +
      `${thrown.raised}, undelivered ${thrown.undelivered}, and left the log at ${throwing.position()}`);
  }
  // The error is the log's own: it counts entries and names no subscription. The handler saw
  // its entry, so no position is given to open the log again at.
  if (thrown.fragments !== 1 || thrown.committedLog !== true || thrown.undeliveredAt !== null ||
      !thrown.message.includes("the entry handler threw on 1 of the 1 entry") || /subscription/i.test(thrown.message)) {
    fail(`a committed log's HandlerError has fragments ${thrown.fragments}, committedLog ${thrown.committedLog}, ` +
      `undeliveredAt ${thrown.undeliveredAt} and says: ${thrown.message}`);
  }
  for (let i = 0; i < 50; i++) {
    if (throwing.poll(throws, 16) !== 0) fail("the entry whose handler threw was delivered again");
  }
  throwing.close();

  // From the next position nothing is committed yet: no entry, and no entry before it. A
  // replay's image joins a moment after the open, so polls made at once would pass before
  // the image is there, whatever the open did with the position. So a second log is opened
  // at 0 beside this one, on the same client, and both are polled until that one has
  // delivered the entry at 0 and this one has taken the member's commit position as its
  // bound, which a poll does once the log's image has joined (`CommittedLog::poll`,
  // crates/cavalry-client/src/committed_log.rs): by then this log's replay, asked for first,
  // has had the time the other's took to join and deliver.
  const rest = openLog(client, NEXT);
  if (rest.position() !== NEXT) fail(`a log opened at ${NEXT} began at ${rest.position()}`);
  const beside = openLog(client, 0);
  const atNext = [];
  const atStart = [];
  const onNext = (body, header, position) => { atNext.push(position); };
  const onStart = (body, header, position) => { atStart.push(position); };
  const untilBoth = Date.now() + WAIT_MS;
  while (atStart.length === 0 || rest.commitPosition() < NEXT) {
    const counts = [rest.poll(onNext, 16), beside.poll(onStart, 16)];
    if (Math.min(...counts) < 0) {
      fail(`the polls of the logs at ${NEXT} and at 0 returned ${counts}: faults ${JSON.stringify(rest.fault())} ` +
        `and ${JSON.stringify(beside.fault())}`);
    }
    if (Date.now() > untilBoth) {
      fail(`the log at 0 beside the log at ${NEXT} delivered ${JSON.stringify(atStart)} and the log at ${NEXT} took ` +
        `the bound ${rest.commitPosition()}, not the entry at 0 and ${NEXT}, within ${WAIT_MS / 1000} s`);
    }
    if (counts[0] === 0 && counts[1] === 0) sleep(IDLE_MS);
  }
  for (let i = 0; i < 50; i++) {
    if (rest.poll(onNext, 16) < 0) fail(`the poll of the log at ${NEXT} stopped on fault ${JSON.stringify(rest.fault())}`);
  }
  if (atStart[0] !== 0 || atNext.length !== 0 || rest.position() !== NEXT) {
    fail(`a log opened at ${NEXT} delivered the entries at ${JSON.stringify(atNext)} and is at ${rest.position()}, ` +
      `while a log opened at 0 beside it delivered the entries at ${JSON.stringify(atStart)}. An entry below ` +
      `${NEXT} is one the open did not start past; one at ${NEXT} or above is a new term's, of a cluster that ` +
      `elected again, which the members' output, printed by run.sh, shows`);
  }
  beside.close();
  // A log that outlives its client: its poll stops on the closed fault, and its close frees
  // it with no client to stop the replay through.
  client.close();
  if (rest.poll(onEntry, 16) !== -1 || rest.fault().code !== constants.CAVALRY_ERR_FAULT_CLOSED) {
    fail(`a poll after the client's close did not stop on the closed fault: ${JSON.stringify(rest.fault())}`);
  }
  rest.close();
  say(`twenty logs opened and closed with no poll between left the client open; a throwing handler's entry is ` +
    `not delivered again; nothing at position ${NEXT} while a log at 0 beside it read the entry at 0; a log ` +
    `closed after its client`);
}

try {
  main();
} catch (thrown) {
  if (thrown instanceof cavalry.CavalryError) fail(`${thrown.fn} failed with error ${thrown.code}: ${thrown.abiMessage}`);
  if (thrown instanceof cavalry.HandlerError) fail(`the handler threw: ${thrown.cause}`);
  throw thrown;
}
