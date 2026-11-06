#!/usr/bin/env node
"use strict";
/**
 * The Node client's conformance runner (clients/README.md; ADR 0046, card 206): the
 * ten-minute path's shape through `clients/node`, as `conformance/python/quickstart.py` runs
 * it through `clients/python`. It checks that a package of another ABI version fails when it
 * loads the addon. Against a driver at `CAVALRY_DIR` it checks the ABI version both ways,
 * records stream 1001 on `cavalry:ipc`, publishes 1,000 messages of 64 bytes by claim and
 * commit, each carrying its index and a pattern, and reads them back through the fragment
 * handler once and in order, with the header's type, flags, version, frame length, term
 * offset and session id checked against spec/frame.md; waits for the recording to sync to
 * the stream's end and stops it; checks where a handler's exception goes, that `offer`
 * refuses what is not bytes and that a view or a header kept past its call reads as empty,
 * on stream 1003 (`edges`); does the same 1,000 over `cavalry:udp` on the loopback; and
 * replays the recording onto stream 2001, each frame at the position the recording holds it
 * at, with the end reported. Every wait is bounded at 10 s, and a wait that runs out says
 * how far it got. It exits 1 with the function, the code and `cavalry_errmsg` when a call
 * failed, or with the code's name when a result code did.
 *
 * Then the report, printed and never gated, from two rounds that check nothing else. The
 * latency round (`latency`): 100,000 messages over `cavalry:ipc` on stream 1004, one at a
 * time on this thread, each carrying the publisher's `process.hrtime.bigint()` from before
 * its commit, which the handler subtracts from the clock it reads first; the one-way p50,
 * p99, p99.99 and max, nearest rank, so ten samples lie above the p99.99. The allocation
 * round (`allocations`): 10,000 messages on stream 1002 whose handler only counts and whose
 * publisher copies one constant, with V8's sampling heap profiler on for this round alone
 * (`inspector`'s `HeapProfiler`, the objects a collection freed included): the bytes
 * allocated per message on this side of the boundary, the client's code and the runtime's
 * together, an unbiased estimate from the samples; and, when `--expose-gc` is on, the bytes
 * the heap kept per message after a full collection, read from `process.memoryUsage()` with
 * nothing else between. clients/node/README.md lists what a message makes by construction.
 * Node's standard library only (ADR 0003).
 */

const dgram = require("dgram");
const fs = require("fs");
const inspector = require("inspector");
const os = require("os");
const path = require("path");

const ROOT = path.join(__dirname, "..", "..");
const CLIENT = path.join(ROOT, "clients", "node");
const cavalry = require(CLIENT);
const { addon, constants } = cavalry;

const MESSAGES = 1000;
const LATENCY_MESSAGES = 100000;
const ALLOCATION_MESSAGES = 10000;
const BODY = 64;
const STREAM = 1001;
const REPLAY_STREAM = 2001;
const WAIT_MS = 10000;
const CODE_NAMES = new Map([
  [cavalry.NOT_CONNECTED, "NOT_CONNECTED"], [cavalry.BACK_PRESSURED, "BACK_PRESSURED"],
  [cavalry.ADMIN_ACTION, "ADMIN_ACTION"], [cavalry.CLOSED, "CLOSED"],
  [cavalry.MAX_POSITION_EXCEEDED, "MAX_POSITION_EXCEEDED"], [cavalry.ERROR, "ERROR"],
]);

function fail(what) {
  process.stderr.write(`conformance/node: ${what}\n`);
  process.exit(1);
}

function say(what) {
  process.stdout.write(`conformance/node: ${what}\n`);
}

function deadline() {
  return Date.now() + WAIT_MS;
}

function codeName(code) {
  return CODE_NAMES.has(code) ? CODE_NAMES.get(code) : String(code);
}

function errmsg() {
  return addon.cavalry_errmsg() || "";
}

function same(a, b) {
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

function range(n, from = 0) {
  return Array.from({ length: n - from }, (_, i) => from + i);
}

/** What a subscription has received: the next index expected, and the frames' term ids
 * and offsets kept from the handler for the positions. */
class Receiver {
  constructor(name, subscription) {
    this.name = name;
    this.subscription = subscription;
    this.next = 0;
    this.sessionId = null;
    this.termIds = [];
    this.termOffsets = [];
    this.positions = [];
    this.constants = null;
    this.onFragment = this.onFragment.bind(this);
  }

  onFragment(body, header) {
    if (this.next >= MESSAGES) fail(`${this.name}: a fragment after all ${MESSAGES} messages`);
    const index = Number(body.readBigUInt64LE(0));
    if (index !== this.next) fail(`${this.name}: expected message ${this.next}, got ${index}`);
    if (body.length !== BODY) fail(`${this.name}: message ${index} has ${body.length} bytes, not ${BODY}`);
    for (let b = 8; b < BODY; b++) {
      if (body[b] !== ((index + b) & 0xff)) {
        fail(`${this.name}: message ${index} byte ${b} is ${body[b]}, not ${(index + b) & 0xff}`);
      }
    }
    const type = header.type;
    if (type === undefined) fail(`${this.name}: cavalry_header_values refused message ${index}'s header inside the handler`);
    // The header against spec/frame.md: a data frame, unfragmented, version 0, the body
    // plus the 32-byte header, aligned to 32 in its term.
    const { flags, version, frameLength, termOffset } = header;
    if (type !== constants.CAVALRY_FRAME_TYPE_DATA || flags !== constants.CAVALRY_FLAGS_UNFRAGMENTED || version !== 0
        || frameLength !== 32 + BODY || termOffset % 32 !== 0) {
      fail(`${this.name}: message ${index}'s header reads type ${type}, flags ${flags}, version ${version}, frame ` +
           `length ${frameLength}, term offset ${termOffset}`);
    }
    if (this.sessionId === null) {
      this.sessionId = header.sessionId;
    } else if (header.sessionId !== this.sessionId) {
      fail(`${this.name}: message ${index} from session ${header.sessionId}, the stream's is ${this.sessionId}`);
    }
    this.termIds.push(header.termId);
    this.termOffsets.push(termOffset);
    this.next += 1;
  }

  /** The constants of the image the frames came from, once a poll has read one, for the
   * positions (spec/c-abi.md, "A frame's position"); the string is copied now. */
  takeConstants() {
    if (this.constants !== null || this.sessionId === null) return;
    for (const image of this.subscription.images()) {
      if (image.sessionId === this.sessionId) {
        this.constants = image;
        return;
      }
    }
  }

  computePositions() {
    if (this.constants === null) fail(`${this.name}: no image carried session ${this.sessionId} after ${this.next} messages`);
    const c = this.constants;
    for (let i = 0; i < this.termIds.length; i++) {
      const termCount = (this.termIds[i] - c.initialTermId) | 0; // 32-bit two's complement
      this.positions.push(termCount * 2 ** c.positionBitsToShift + this.termOffsets[i]);
    }
  }

  expectNoMore() {
    const count = this.subscription.poll(this.onFragment, 16);
    if (count !== 0) fail(`${this.name}: ${count} more fragment(s) after all ${MESSAGES} messages`);
  }
}

/** A claim refused with `position`, below zero: fail at once on a code that does not pass,
 * ERROR with the ABI's error, CLOSED or MAX_POSITION_EXCEEDED, or once the round's 10 s have
 * passed, saying how far the round got; else return, to claim again. */
function refused(name, position, sent, until, total = MESSAGES) {
  if (position === cavalry.ERROR) {
    throw new cavalry.CavalryError("cavalry_publication_try_claim", addon.cavalry_errcode(), errmsg());
  }
  if (position === cavalry.CLOSED || position === cavalry.MAX_POSITION_EXCEEDED) {
    fail(`${name}: tryClaim returned ${codeName(position)} (${position}) after ${sent} of ${total} messages`);
  }
  if (Date.now() > until) {
    fail(`${name}: published ${sent} of ${total} in ${WAIT_MS / 1000} s; the last claim returned ` +
         `${codeName(position)} (${position})`);
  }
}

/** One poll of up to 16 fragments: the count, or a failure with the subscription's fault,
 * saying how far the round got. */
function pollOrFail(name, subscription, handler, received, total = MESSAGES) {
  const count = subscription.poll(handler, 16);
  if (count < 0) {
    const fault = subscription.fault();
    fail(`${name}: poll stopped on fault ${fault.code} of session ${fault.sessionId} at position ${fault.position} ` +
         `after ${received} of ${total} messages; ${errmsg()}`);
  }
  return count;
}

function writeMessage(claim, i) {
  claim.writeBigUInt64LE(BigInt(i), 0);
  for (let b = 8; b < BODY; b++) claim[b] = (i + b) & 0xff;
}

function publishAndReceive(publication, receiver) {
  let until = deadline();
  let i = 0;
  while (i < MESSAGES) {
    // The image joins on a poll; take its constants once a frame from it is read.
    pollOrFail(receiver.name, receiver.subscription, receiver.onFragment, receiver.next);
    receiver.takeConstants();
    const position = publication.tryClaim(BODY);
    if (position < 0) {
      refused(receiver.name, position, i, until);
      continue;
    }
    writeMessage(publication.claim, i);
    if (publication.commit() !== position) fail(`${receiver.name}: commit did not return the claim's position ${position}`);
    i += 1;
  }
  until = deadline();
  while (receiver.next < MESSAGES) {
    pollOrFail(receiver.name, receiver.subscription, receiver.onFragment, receiver.next);
    receiver.takeConstants();
    if (Date.now() > until) fail(`${receiver.name}: received ${receiver.next} of ${MESSAGES} in ${WAIT_MS / 1000} s`);
  }
  receiver.expectNoMore();
  receiver.computePositions();
}

/** Offer `message`, retrying a refusal that passes, NOT_CONNECTED, BACK_PRESSURED or
 * ADMIN_ACTION, for up to 10 s; the position. */
function offerOrFail(name, publication, message, sent) {
  const until = deadline();
  for (;;) {
    const position = publication.offer(message);
    if (position >= 0) return position;
    if (position === cavalry.CLOSED || position === cavalry.MAX_POSITION_EXCEEDED || position === cavalry.ERROR
        || Date.now() > until) {
      fail(`${name}: offer returned ${codeName(position)} (${position}) after ${sent} messages`);
    }
  }
}

function message(i) {
  const m = Buffer.alloc(BODY);
  m.writeBigUInt64LE(BigInt(i), 0);
  return m;
}

function indexOf(body) {
  return Number(body.readBigUInt64LE(0));
}

/** The client's edges, on stream 1003. Where a handler's exception goes
 * (clients/node/README.md, "The exception"): ten messages offered, then one poll of ten
 * whose handler throws on message 3. The ABI's poll consumes all ten, so every one must
 * reach the handler, once and in order, and poll must throw HandlerError with `fragments`
 * 10, `raised` 1 and the Error as its cause. Then a poll whose handler throws an Error on
 * one message and a string on the next: HandlerError with `raised` 2 and the Error, the
 * first, as its cause. Then what `offer` refuses: a string, a number, a plain object and an
 * array are refused with TypeError and nothing is published. Then the lifetimes: a claim's
 * Buffer kept past its commit, and a body and a header kept past their handler call, read
 * as empty. Then the refusals of spec/c-abi.md, "Calls during a claim or a poll", which
 * leave the wrapper as they found it: a poll from inside a handler of its own subscription,
 * a second claim with one outstanding, and a close with a claim outstanding, which releases
 * its Buffer. */
function edges(client) {
  let name = "handler exceptions";
  const subscription = client.addSubscription("cavalry:ipc", STREAM + 2);
  const publication = client.addPublication("cavalry:ipc", STREAM + 2);
  const seen = [];
  const unexpected = () => fail(`${name}: a fragment the test did not expect, after ${seen}`);

  let until = deadline();
  for (;;) {
    subscription.poll(unexpected, 10); // nothing is offered yet; the image joins on a poll
    const position = publication.offer(message(0));
    if (position >= 0) break;
    if (position === cavalry.CLOSED || position === cavalry.MAX_POSITION_EXCEEDED || position === cavalry.ERROR
        || Date.now() > until) {
      fail(`${name}: the first offer returned ${codeName(position)} (${position})`);
    }
  }
  for (let i = 1; i < 10; i++) offerOrFail(name, publication, message(i), i);

  const throwsOn3 = (body) => {
    const index = indexOf(body);
    seen.push(index);
    if (index === 3) throw new Error("message 3");
  };
  let error = null;
  try {
    subscription.poll(throwsOn3, 10);
  } catch (thrown) {
    error = thrown;
  }
  if (error === null) fail(`${name}: a handler that threw on message 3 did not make poll throw HandlerError; it saw ${seen}`);
  if (!(error instanceof cavalry.HandlerError) || !same(seen, range(10)) || error.fragments !== 10 || error.raised !== 1
      || !(error.cause instanceof Error) || error.cause.message !== "message 3") {
    fail(`${name}: the handler saw ${seen}; poll threw ${error} with fragments ${error.fragments}, raised ` +
         `${error.raised}, cause ${error.cause}; expected messages 0 to 9 once and in order, HandlerError, 10, 1 and the Error`);
  }
  if (!error.message.includes("Fix the handler") || !error.message.endsWith("See clients/node/README.md#the-exception")) {
    fail(`${name}: HandlerError's message does not say what to do and where it is explained: ${error.message}`);
  }

  offerOrFail(name, publication, message(10), 10);
  offerOrFail(name, publication, message(11), 11);
  const throwsTwice = (body) => {
    const index = indexOf(body);
    seen.push(index);
    if (index === 10) throw new Error("message 10");
    throw "message 11"; // eslint-disable-line no-throw-literal
  };
  error = null;
  try {
    subscription.poll(throwsTwice, 10);
  } catch (thrown) {
    error = thrown;
  }
  if (!(error instanceof cavalry.HandlerError) || !same(seen, range(12)) || error.fragments !== 2 || error.raised !== 2
      || !(error.cause instanceof Error) || error.cause.message !== "message 10") {
    fail(`${name}: poll threw ${error} and the handler saw ${seen}; expected HandlerError with fragments 2, raised 2 ` +
         `and the first Error as its cause, and messages 0 to 11`);
  }
  if (subscription.poll(unexpected, 10) !== 0) fail(`${name}: a fragment after all 12 messages`);
  say(`${name}: a handler that threw on message 3 of a poll of 10 saw all 10 once, in order, and poll threw ` +
      `HandlerError (fragments 10, raised 1, the Error as its cause); a handler that threw an Error and then a string ` +
      `in one poll made poll throw HandlerError with raised 2 and the Error as its cause`);

  // What offer refuses: anything that is not an ArrayBuffer, a Buffer, a typed array or a
  // DataView, which the addon refuses before the ABI sees it.
  name = "offer";
  for (const notBytes of ["hello", 5, { length: 5 }, [1, 2, 3]]) {
    let position;
    try {
      position = publication.offer(notBytes);
    } catch (thrown) {
      if (thrown instanceof TypeError) continue;
      fail(`${name}: refused ${JSON.stringify(notBytes)} with ${thrown}, not a TypeError`);
    }
    fail(`${name}: took ${JSON.stringify(notBytes)} and returned ${position}; it takes bytes only`);
  }
  if (subscription.poll(unexpected, 10) !== 0) fail(`${name}: a refused offer published a fragment`);
  say(`${name}: a string, a number, an object and an array were refused with TypeError and nothing was published`);

  // A Buffer or a header kept past its call, on purpose: the claim's Buffer is detached by
  // its commit, the body's by the trampoline once the handler has returned, so each reads
  // as empty below, a write to it throws, and nothing reaches the log.
  name = "lifetimes";
  until = deadline();
  let position;
  for (;;) {
    position = publication.tryClaim(BODY);
    if (position >= 0) break;
    refused(name, position, 12, until);
  }
  const view = publication.claim;
  view.writeBigUInt64LE(12n, 0);
  publication.commit();
  if (view.length !== 0) fail(`${name}: a claim's Buffer still has ${view.length} bytes after its commit`);
  try {
    view.writeUInt32LE(1, 0);
    fail(`${name}: a claim's Buffer written after its commit`);
  } catch (thrown) {
    if (!(thrown instanceof RangeError)) fail(`${name}: a write to a committed claim's Buffer threw ${thrown}, not a RangeError`);
  }
  const kept = [];
  const keeps = (body, header) => kept.push([body, header, header.sessionId, header.sessionId]);
  until = deadline();
  while (kept.length === 0) {
    pollOrFail(name, subscription, keeps, 0, 1);
    if (Date.now() > until) fail(`${name}: message 12 did not arrive in ${WAIT_MS / 1000} s`);
  }
  const [body, header, sessionId, again] = kept[0];
  if (typeof sessionId !== "number" || sessionId !== again) {
    fail(`${name}: two reads of header.sessionId in one handler call gave ${sessionId} and ${again}; expected the ` +
         `one number the addon reads once per fragment`);
  }
  if (body.length !== 0 || body[0] !== undefined) fail(`${name}: a body read after its handler call has ${body.length} bytes`);
  if (header.sessionId !== undefined || header.frameLength !== undefined) {
    fail(`${name}: a header read after its handler call gave session ${header.sessionId}, frame length ${header.frameLength}`);
  }
  say(`${name}: a claim's Buffer read as empty after its commit and a write to it threw RangeError, a body read as ` +
      `empty after its handler call, a header read after the call gave undefined, and header.sessionId gave the ` +
      `one number on each read`);

  // Calls the ABI refuses during a poll or a claim leave the wrapper as they found it
  // (spec/c-abi.md, "Calls during a claim or a poll").
  name = "refusals";
  for (let i = 13; i < 16; i++) offerOrFail(name, publication, message(i), i);
  const outer = [];
  const nested = [];
  const nestedHandler = () => fail(`${name}: a poll from inside a handler of the same subscription delivered a fragment`);
  const outerHandler = (fragment) => {
    outer.push(indexOf(fragment));
    nested.push([subscription.poll(nestedHandler, 10), addon.cavalry_errcode()]);
  };
  until = deadline();
  while (outer.length < 3) {
    pollOrFail(name, subscription, outerHandler, outer.length, 3);
    if (Date.now() > until) fail(`${name}: received messages ${outer} of 13 to 15 in ${WAIT_MS / 1000} s`);
  }
  if (!same(outer, [13, 14, 15]) || nested.length !== 3
      || !nested.every(([result, code]) => result === -1 && code === constants.CAVALRY_ERR_POLLING)) {
    fail(`${name}: the outer handler saw ${outer} and the polls from inside it returned (result, code) ` +
         `${JSON.stringify(nested)}; expected messages 13 to 15, and -1 with CAVALRY_ERR_POLLING ` +
         `(${constants.CAVALRY_ERR_POLLING}) each time`);
  }

  until = deadline();
  for (;;) {
    position = publication.tryClaim(BODY);
    if (position >= 0) break;
    refused(name, position, 16, until);
  }
  const first = publication.claim;
  first.writeBigUInt64LE(16n, 0);
  const secondClaim = publication.tryClaim(BODY);
  const code = addon.cavalry_errcode();
  if (secondClaim !== cavalry.ERROR || code !== constants.CAVALRY_ERR_CLAIM_OUTSTANDING || publication.claim !== first) {
    fail(`${name}: a tryClaim with a claim outstanding returned ${codeName(secondClaim)} with error ${code}, and claim ` +
         `is ${publication.claim}; expected ERROR with CAVALRY_ERR_CLAIM_OUTSTANDING and the outstanding claim's Buffer`);
  }
  if (publication.commit() !== position || publication.claim !== null) {
    fail(`${name}: the outstanding claim's commit did not return its position ${position} and release its Buffer`);
  }
  if (first.length !== 0) fail(`${name}: a claim's Buffer still has ${first.length} bytes after its commit, past a refused second claim`);
  const received = [];
  const index = (fragment) => received.push(indexOf(fragment));
  until = deadline();
  while (received.length === 0) {
    pollOrFail(name, subscription, index, 16, 17);
    if (Date.now() > until) fail(`${name}: message 16 did not arrive in ${WAIT_MS / 1000} s`);
  }
  if (!same(received, [16])) fail(`${name}: received ${received} for the claim committed past a refused one; expected message 16`);

  until = deadline();
  for (;;) {
    position = publication.tryClaim(BODY);
    if (position >= 0) break;
    refused(name, position, 17, until);
  }
  const outstanding = publication.claim;
  publication.close();
  if (publication.claim !== null) fail(`${name}: the publication's close left claim ${publication.claim}`);
  if (outstanding.length !== 0) fail(`${name}: a claim's Buffer still has ${outstanding.length} bytes after its publication's close`);
  pollOrFail(name, subscription, unexpected, 17, 17); // the close aborted the claim: padding, never a fragment
  subscription.close();
  say(`${name}: a poll from inside a handler of its own subscription returned -1 with CAVALRY_ERR_POLLING and the ` +
      `outer handler saw messages 13 to 15; a tryClaim with a claim outstanding returned ERROR with ` +
      `CAVALRY_ERR_CLAIM_OUTSTANDING, claim stayed the first Buffer and its commit released it; a close with a claim ` +
      `outstanding released its Buffer`);
}

/** The latency round, for the report: 100,000 messages over cavalry:ipc on stream 1004, one
 * at a time on this thread. The publisher writes `process.hrtime.bigint()` into the claim
 * just before its commit; the handler reads the clock first and keeps the difference. The
 * profiler is off. The latencies, in order. */
function latency(client) {
  const name = "the latency round";
  const subscription = client.addSubscription("cavalry:ipc", STREAM + 3);
  const publication = client.addPublication("cavalry:ipc", STREAM + 3);
  const latencies = new Float64Array(LATENCY_MESSAGES);
  let received = 0;
  const onFragment = (body) => {
    const now = process.hrtime.bigint();
    latencies[received] = Number(now - body.readBigUInt64LE(0));
    received += 1;
  };
  let until = deadline();
  for (let i = 0; i < LATENCY_MESSAGES; i++) {
    for (;;) {
      const position = publication.tryClaim(BODY);
      if (position >= 0) break;
      refused(name, position, i, until, LATENCY_MESSAGES);
      pollOrFail(name, subscription, onFragment, received, LATENCY_MESSAGES); // the image joins on a poll
    }
    publication.claim.writeBigUInt64LE(process.hrtime.bigint(), 0);
    publication.commit();
    while (received <= i) {
      pollOrFail(name, subscription, onFragment, received, LATENCY_MESSAGES);
      if (Date.now() > until) {
        fail(`${name}: received ${received} of ${LATENCY_MESSAGES}; message ${i} did not arrive in ${WAIT_MS / 1000} s`);
      }
    }
    until = deadline(); // 10 s for each message, outside the interval it measures
  }
  if (subscription.poll(onFragment, 16) !== 0) fail(`${name}: a fragment after all ${LATENCY_MESSAGES} messages`);
  publication.close();
  subscription.close();
  return latencies;
}

/** The allocation round, for the report: 10,000 messages over cavalry:ipc on stream 1002
 * whose handler only counts and whose publisher copies one constant, so that the checks'
 * objects are not in the count, with V8's sampling heap profiler on for this round alone,
 * the objects a collection freed included, and a full collection before and after when
 * `--expose-gc` is on. The counts are read in this order, so that the profile, the large
 * object the profiler returns, is not in the kept count: the collection, the heap used,
 * the profiler on; the round; the heap used, the collection, the heap used, the profiler
 * off. {measured, sampledBytes, keptBytes (null without --expose-gc), heapBefore, heapAfter}. */
function allocations(client) {
  const name = "the allocation round";
  const subscription = client.addSubscription("cavalry:ipc", STREAM + 1);
  const publication = client.addPublication("cavalry:ipc", STREAM + 1);
  let received = 0;
  const count = () => { received += 1; };
  const constant = Buffer.from(range(BODY));
  let until = deadline();
  for (;;) {
    pollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
    const position = publication.tryClaim(BODY);
    if (position >= 0) break;
    refused(name, position, 0, until, ALLOCATION_MESSAGES);
  }
  constant.copy(publication.claim);
  publication.commit();
  let sent = 1;
  pollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);

  const session = new inspector.Session();
  session.connect();
  const post = (method, params) => {
    let out = null;
    session.post(method, params, (error, result) => {
      if (error) fail(`${name}: ${method}: ${error.message}`);
      out = result === undefined ? {} : result;
    });
    if (out === null) fail(`${name}: ${method} did not answer within the call`);
    return out;
  };
  const collect = typeof global.gc === "function" ? global.gc : null;
  if (collect) collect();
  const heapBefore = process.memoryUsage().heapUsed;
  post("HeapProfiler.startSampling", { samplingInterval: 64, includeObjectsCollectedByMajorGC: true,
                                       includeObjectsCollectedByMinorGC: true });
  const receivedBefore = received;
  until = deadline();
  while (sent < ALLOCATION_MESSAGES) {
    const position = publication.tryClaim(BODY);
    if (position >= 0) {
      constant.copy(publication.claim);
      publication.commit();
      sent += 1;
    } else {
      refused(name, position, sent, until, ALLOCATION_MESSAGES);
    }
    pollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
  }
  while (received < ALLOCATION_MESSAGES) {
    pollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
    if (Date.now() > until) fail(`${name}: received ${received} of ${ALLOCATION_MESSAGES} in ${WAIT_MS / 1000} s`);
  }
  const heapAfter = process.memoryUsage().heapUsed;
  let keptBytes = null;
  if (collect) {
    collect();
    keptBytes = process.memoryUsage().heapUsed - heapBefore;
  }
  const { profile } = post("HeapProfiler.stopSampling"); // last: the profile it returns is a large object
  session.disconnect();

  const measured = ALLOCATION_MESSAGES - receivedBefore;
  let sampledBytes = 0;
  const walk = (node) => {
    sampledBytes += node.selfSize;
    for (const child of node.children) walk(child);
  };
  walk(profile.head);
  if (subscription.poll(count, 16) !== 0) fail(`${name}: a fragment after all ${ALLOCATION_MESSAGES} messages`);
  publication.close();
  subscription.close();
  return { measured, sampledBytes, keptBytes, heapBefore, heapAfter };
}

/** The nearest-rank percentile `parts / whole` of sorted values: the smallest value with at
 * least that share of the values at or below it, and how many values lie above it. */
function percentile(sorted, parts, whole) {
  const rank = Math.max(1, Math.ceil(sorted.length * parts / whole));
  return [sorted[rank - 1], sorted.length - rank];
}

function withCommas(n) {
  return Math.round(n).toLocaleString("en-US");
}

/** The load-time check (spec/c-abi.md, "Versions and what a reader rejects"): a package whose
 * ABI_VERSION is not the library's fails when it is required, before any other function,
 * with CavalryError naming both numbers. The package is the committed index.js with its
 * version one higher and the addon's path made absolute, required from a directory of its
 * own, as conformance/python/quickstart.py patches `_abi.CAVALRY_ABI_VERSION`. */
function refusedAtLoad() {
  const version = cavalry.ABI_VERSION;
  const source = fs.readFileSync(path.join(CLIENT, "index.js"), "utf8");
  const relative = 'require(path.join(__dirname, "build", "Release", "cavalry_addon.node"))';
  const bumped = source
    .replace(/^const ABI_VERSION = (\d+);$/m, (_, found) => `const ABI_VERSION = ${Number(found) + 1};`)
    .replace(relative, `require(${JSON.stringify(path.join(CLIENT, "build", "Release", "cavalry_addon.node"))})`);
  if (bumped === source || !bumped.includes(`const ABI_VERSION = ${version + 1};`) || bumped.includes(relative)) {
    fail("could not make a package of the next ABI version from clients/node/index.js; keep its `const ABI_VERSION` " +
         "line and the addon's require as the generator writes them, so this check can find them");
  }
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cavalry-node-abi-"));
  let refusal = null;
  try {
    fs.writeFileSync(path.join(dir, "index.js"), bumped);
    try {
      require(path.join(dir, "index.js"));
    } catch (thrown) {
      refusal = thrown;
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  if (refusal === null) fail(`a package of ABI version ${version + 1} loaded a library of version ${version}`);
  if (refusal.name !== "CavalryError" || refusal.code !== constants.CAVALRY_ERR_ABI_VERSION
      || !refusal.message.includes(`is ABI version ${version} and`)
      || !refusal.message.includes(`from a header of ABI version ${version + 1};`)) {
    fail(`a package of ABI version ${version + 1} was refused with ${refusal}; expected CavalryError with ` +
         `CAVALRY_ERR_ABI_VERSION naming both versions`);
  }
  say(`ABI version: a package of version ${version + 1} was refused when required: ${refusal.message}`);
}

function freeUdpPort(callback) {
  const socket = dgram.createSocket("udp4");
  socket.bind(0, "127.0.0.1", () => {
    const { port } = socket.address();
    socket.close(() => callback(port));
  });
}

function main(udpPort) {
  refusedAtLoad();

  // What a reader rejects, both ways: another version is refused with the code, and the
  // package's own version is not.
  const [mismatch, context] = addon.cavalry_context_init_v(cavalry.ABI_VERSION + 1);
  if (mismatch === 0 || addon.cavalry_errcode() !== constants.CAVALRY_ERR_ABI_VERSION) {
    if (context !== null) addon.cavalry_context_close(context);
    fail("a mismatched ABI version was not refused with CAVALRY_ERR_ABI_VERSION");
  }
  if (cavalry.abiVersion() !== cavalry.ABI_VERSION) {
    fail(`the library's ABI version is ${cavalry.abiVersion()}, the package's ${cavalry.ABI_VERSION}`);
  }

  const client = new cavalry.Client();
  let latencies;
  let allocation;
  try {
    say(`client ${client.clientId} on ${client.dir}, library ${cavalry.version()}, ABI version ${cavalry.abiVersion()}`);

    // 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
    // back, once and in order.
    const ipcSub = client.addSubscription("cavalry:ipc", STREAM);
    const ipcPub = client.addPublication("cavalry:ipc", STREAM);
    const recording = client.startRecording("cavalry:ipc", STREAM); // the publication first: it records a live one
    const ipc = new Receiver("cavalry:ipc", ipcSub);
    publishAndReceive(ipcPub, ipc);
    const end = ipcPub.position();
    say(`${MESSAGES} messages over cavalry:ipc by session ${ipc.sessionId} received once, in order; the stream ends at ${end}`);
    let until = deadline();
    while (recording.syncedPosition() < end) {
      if (Date.now() > until) fail(`the recording synced ${recording.syncedPosition()} of ${end} in ${WAIT_MS / 1000} s`);
    }
    client.stopRecording(recording.recordingId);
    const recordingId = recording.recordingId;
    recording.close();
    ipcPub.close();

    edges(client);

    // The report's two rounds, which check nothing else.
    latencies = latency(client).sort();
    allocation = allocations(client);

    // 2. The same 1,000 over cavalry:udp on the loopback.
    const udpChannel = `cavalry:udp?endpoint=127.0.0.1:${udpPort}`;
    const udpSub = client.addSubscription(udpChannel, STREAM);
    const udpPub = client.addPublication(udpChannel, STREAM);
    const udp = new Receiver(udpChannel, udpSub);
    publishAndReceive(udpPub, udp);
    say(`${MESSAGES} messages over ${udpChannel} by session ${udp.sessionId} received once, in order, from ` +
        `${udp.constants.sourceIdentity}`);
    udpPub.close();
    udpSub.close();

    // 3. Replay the recording onto stream 2001: the same checks, each frame at the
    // position the recording holds it at (ADR 0030), and the end reported.
    const replaySub = client.addSubscription("cavalry:ipc", REPLAY_STREAM);
    const replay = client.startReplay(recordingId, constants.CAVALRY_NULL_POSITION, constants.CAVALRY_NULL_LENGTH,
                                      REPLAY_STREAM, "cavalry:ipc");
    const replayed = new Receiver("replay", replaySub);
    until = deadline();
    let endedAt;
    let reason;
    for (;;) {
      pollOrFail("replay", replaySub, replayed.onFragment, replayed.next);
      replayed.takeConstants();
      const state = replay.ended();
      endedAt = state.position;
      reason = state.reason;
      if (replayed.next >= MESSAGES && state.ended) break;
      if (Date.now() > until) {
        if (state.ended) {
          fail(`replayed ${replayed.next} of ${MESSAGES} in ${WAIT_MS / 1000} s; the replay ended at ${endedAt} with reason ${reason}`);
        }
        fail(`replayed ${replayed.next} of ${MESSAGES} in ${WAIT_MS / 1000} s; the replay has not ended, at position ${replay.position()}`);
      }
    }
    replayed.expectNoMore();
    replayed.computePositions();
    if (endedAt !== end || reason !== constants.CAVALRY_REPLAY_STOP_ENDED) {
      fail(`the replay ended at ${endedAt} with reason ${reason}, not at ${end} with ${constants.CAVALRY_REPLAY_STOP_ENDED}`);
    }
    for (let i = 0; i < MESSAGES; i++) {
      if (replayed.positions[i] !== ipc.positions[i]) {
        fail(`message ${i} replayed at ${replayed.positions[i]}, recorded at ${ipc.positions[i]}`);
      }
    }
    say(`${MESSAGES} messages replayed on stream ${REPLAY_STREAM} at the positions the recording holds them, ended at ${endedAt}`);
    replay.close();
    replaySub.close();
    ipcSub.close();
  } finally {
    client.close();
  }

  // The report, never gated: the laptop's or the runner's cost of this side of the
  // boundary (clients/node/README.md, "The runtime's side").
  const [p50] = percentile(latencies, 50, 100);
  const [p99] = percentile(latencies, 99, 100);
  const [p9999, above] = percentile(latencies, 9999, 10000);
  say(`report: one-way latency over ${withCommas(latencies.length)} cavalry:ipc messages, one at a time on one thread, ` +
      `the profiler off, nearest rank: p50 ${withCommas(p50)} ns, p99 ${withCommas(p99)} ns, p99.99 ${withCommas(p9999)} ns ` +
      `(${above} samples above it), max ${withCommas(latencies[latencies.length - 1])} ns`);
  const { measured, sampledBytes, keptBytes, heapBefore, heapAfter } = allocation;
  const kept = keptBytes === null ? "the bytes kept after a collection not read, run with --expose-gc" :
    `${(keptBytes / measured).toFixed(1)} bytes kept per message after a full collection (process.memoryUsage().heapUsed, ` +
    `${withCommas(heapBefore)} before, ${withCommas(heapAfter)} after the round, nothing read between)`;
  say(`report: allocations per message, this side of the boundary, over ${withCommas(measured)} messages whose handler ` +
      `counts and whose publisher copies a constant: ${(sampledBytes / measured).toFixed(1)} bytes allocated (V8's ` +
      `sampling heap profiler at 64 bytes, an unbiased estimate, the objects a collection freed included); ${kept}`);
  say("ok");
  return 0;
}

freeUdpPort((udpPort) => {
  try {
    process.exitCode = main(udpPort);
  } catch (thrown) {
    if (thrown instanceof cavalry.CavalryError) fail(`${thrown.fn} failed with error ${thrown.code}: ${thrown.abiMessage}`);
    if (thrown instanceof cavalry.HandlerError) fail(`the fragment handler threw: ${thrown.cause}`);
    throw thrown;
  }
});
