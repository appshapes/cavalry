#!/usr/bin/env node
"use strict";
/**
 * The Node client's cluster round (spec/c-abi.md, versions 4, 5 and 7; ADR 0122, ADR 0123,
 * ADR 0124, ADR 0157): `Client.clusterService` and `ClusterService` beside each of three cluster members
 * that `conformance/node/run.sh` started with `cavalry run`, and `Client.connectCluster` and
 * `ClusterSession` on the runner's first driver, the one at CAVALRY_DIR, as
 * `conformance/python/cluster.py` runs it through `clients/python`.
 *
 *     cluster.js <cluster directory> <members string> <response channel>
 *
 * The members' drivers are <cluster directory>/member-0, -1 and -2. The round is the C
 * runner's, `conformance/c/cluster.c`, through the client, and then the client's own edges.
 *
 * The C runner's checks: a reply from outside the handler is refused; each service reads an
 * entry before the session connects, since a service answers no entry that its member held
 * when the service first opened the member's log; one message is handed to all three services
 * as one entry, with one position, term, time and session; one service answers it, the one
 * beside the leader, and the two others are answered CAVALRY_CLUSTER_REPLY_NOT_ANSWERING; the
 * session reads one reply, that service's, with the entry's term and the cluster's time; each
 * service holds one session, and its position is past the entry.
 *
 * The client's own edges: what `offer` refuses; the calls the ABI refuses from inside a
 * service's handler, on the service and on the client its poll holds, and from inside the
 * session's, each made after the thread's error was set to another code, so that a refusal
 * that answers what an open handle answers is told from an answer; the one object of values a
 * handler is handed, which reads undefined once the call has returned, as its body reads
 * empty; a service's handler that throws after it replied, one that returns what the ABI
 * refuses as no action, one that returns `true`, as a call-back that says "handled" does,
 * and one that returns a promise, as an `async` handler does, whose
 * entry is aborted, handed over again by the next poll and answered once; a reply too long for
 * a frame and a reply to a session that is not open; a handler that returns BREAK with two
 * entries waiting; a session's handler that throws, whose reply is not handed over again; the
 * program's timers, `scheduleTimer` and `cancelTimer` from inside the handler, as the C
 * runner's round has one: a timer handed to every service once, as an entry of its own type
 * with the timer's id, a BigInt, at one position and no earlier than the deadline its second
 * schedule set, a second after its entry, where its first was an hour away, and not again
 * within a timer refresh interval and more; a timer due at once and cancelled in the same
 * handling handed to none; the least and the greatest int64 as ids, and the least and the
 * greatest integer a number carries exactly, handed back as they are; both calls refused
 * from outside the handler, and an id or a deadline that is no int64, a number with a
 * fraction or outside the safe integers among them, refused before the ABI is called with
 * a message that names the README's section; the
 * session's close as an entry every service is handed; a session and a service closed twice,
 * and a service closed after its client.
 *
 * Then a report, printed and never gated: how many polls the three services made, how many
 * took over a millisecond, and the longest, which is how long a poll held the thread, and with
 * it the event loop (clients/node/README.md, "The call"). And one check after it: the Buffers
 * the round's handlers were handed are all freed by one turn of the event loop
 * (clients/node/README.md, "The runtime's queue").
 *
 * Every wait is bounded at 60 s, the time a cluster may take to commit on a busy host. It
 * exits 1 with what it found when a check fails. Node's standard library only (ADR 0003).
 */

const path = require("path");

const cavalry = require(path.join(__dirname, "..", "..", "clients", "node"));
const { addon, constants } = cavalry;

const MEMBERS = 3;
const WAIT_MS = 60000;
const IDLE_MS = 0.2; // between duty cycles: the members run in processes of their own
const SETTLE_MS = 300; // the time in which a second reply, or a second entry, would come
const MESSAGE = constants.CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE;
const TIMER = constants.CAVALRY_CLUSTER_ENTRY_TIMER;
const SCHEDULED = constants.CAVALRY_CLUSTER_TIMER_SCHEDULED;
const TIMER_MESSAGE = "timer"; // the message at which each service's program schedules its timers
const TIMER_ID = 77; // the timer that is handed over, scheduled as a number and handed back as 77n
const CANCELLED_ID = 78; // the timer that is scheduled and cancelled in the same handling
// How long after the message's entry the timer is due. A second, and not the 50 ms the C
// round and the Python round had first: a service tells its member of a timer only once the
// poll that took the entry has sent the six commands of its first timer, some 25 ms on a
// laptop and more on a busy host, so the event of a timer that was wrongly due at once is
// stamped about as late as a deadline of 50 ms, 49 ms after the message in a run of the
// Python round, and that margin would let it pass. The round prints what it was in its own
// run.
const TIMER_AFTER_MS = 1000;
const SAFE = Number.MAX_SAFE_INTEGER; // the greatest integer a number carries exactly, and an id here
const HOUR_MS = 3600000; // the deadline its first schedule gives it, which the second replaces
const INT64_MIN = -(2n ** 63n);
const INT64_MAX = 2n ** 63n - 1n;
const REFRESH_MS = 1200; // a timer refresh interval of spec/cluster.md, "Values", 1 s, and more
const SENT = constants.CAVALRY_CLUSTER_REPLY_SENT;
const POLLING = constants.CAVALRY_ERR_POLLING;

function fail(what) {
  process.stderr.write(`conformance/node: cluster round: ${what}\n`);
  process.exit(1);
}

function say(what) {
  process.stdout.write(`conformance/node: cluster round: ${what}\n`);
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** A value as text, a BigInt, which JSON has no form for, as its digits and an `n`. */
function show(value) {
  return JSON.stringify(value, (key, v) => (typeof v === "bigint" ? `${v}n` : v));
}

function same(a, b) {
  return show(a) === show(b);
}

/** Sets the thread's error to one no refusal gives, CAVALRY_ERR_NULL, by asking the ABI for
 * the position of a null header, as the committed log round does: a success clears nothing
 * (spec/c-abi.md, "Error codes"), so a refusal that answers what an open handle answers is
 * told from an answer by the code alone, which must have changed from this one to
 * CAVALRY_ERR_POLLING. */
function setAnotherError() {
  if (addon.cavalry_committed_log_entry_position(null) !== -1 || addon.cavalry_errcode() !== constants.CAVALRY_ERR_NULL) {
    fail(`the position of a null header did not answer -1 with CAVALRY_ERR_NULL: error ${addon.cavalry_errcode()}`);
  }
}

/** What each call answers, with the thread's error after it: a `CavalryError` is -1 with its
 * code. */
function refusalsOf(calls) {
  const out = [];
  for (const [name, call] of calls) {
    setAnotherError();
    try {
      out.push([name, call(), addon.cavalry_errcode()]);
    } catch (thrown) {
      if (!(thrown instanceof cavalry.CavalryError)) throw thrown;
      out.push([name, -1, thrown.code]);
    }
  }
  return out;
}

/** A service beside one member: what its handler was handed and what its calls answered. The
 * handler answers a session's message with its member's id, a colon and the message, so that
 * the reply a client reads says whose service sent it. */
class Served {
  constructor(cluster, member) {
    this.member = member;
    this.client = new cavalry.Client(path.join(cluster, `member-${member}`));
    this.service = this.client.clusterService();
    this.entries = []; // every entry taken: [type, position, term, time, session, leader, timed out]
    this.messages = new Map(); // a message's payload -> the entries it was taken as
    this.handed = new Map(); // a message's payload -> the positions it was handed over at, taken or not
    this.codes = new Map(); // a message's payload -> what each reply to it answered
    this.refusals = null; // what the calls from inside the handler answered
    this.throwOn = null; // a payload whose first handling replies and then throws
    this.noActionOn = null; // a payload whose first handling returns what the ABI refuses as no action
    this.promiseOn = null; // a payload whose first handling returns a promise, as an `async` handler does
    this.trueOn = null; // a payload whose first handling returns true, as a call-back that says "handled" does
    this.edgesOn = null; // a payload before whose reply a reply too long and one to no session are made
    this.edges = null;
    this.breakOn = []; // payloads whose entries are taken with BREAK
    this.timers = []; // every timer handed over: [position, correlation id, the event's time]
    this.timerAt = null; // when the last was handed over, by this process's clock
    this.timerCodes = []; // what the schedules and the cancel at TIMER_MESSAGE answered
    this.refusedInside = "no call"; // what a schedule of an id with a fraction from inside the handler did
    this.values = null; // the object of values the handler was handed, the same one for every entry
    this.body = null; // the body of the first session's message, kept past its call
    this.polls = 0; // the report: every poll, those over a millisecond, and the longest
    this.overAMillisecond = 0;
    this.longest = [0n, "no poll"];
    this.onEntry = this.onEntry.bind(this);
  }

  onEntry(body, entry, service) {
    if (service !== this.service) fail("the handler was handed another object than the service that was polled");
    if (this.values === null) this.values = entry;
    if (entry !== this.values) fail("the handler was handed another object of values than the one the service keeps");
    const values = [entry.type, entry.position, entry.leadershipTermId, entry.timestamp, entry.sessionId,
      entry.leaderMemberId, entry.timedOut];
    if (entry.type !== MESSAGE || entry.sessionId < 0) {
      if (body.length !== 0) fail(`an entry that is no session's message has a payload of ${body.length} bytes: ${show(values)}`);
      if (entry.type === TIMER) {
        this.timers.push([entry.position, entry.correlationId, entry.timestamp]);
        this.timerAt = Date.now();
      } else if (entry.correlationId !== 0n) {
        fail(`an entry that is no timer has the correlation id ${show(entry.correlationId)}: ${show(values)}`);
      }
      this.entries.push(values);
      return undefined;
    }
    if (entry.correlationId !== 0n) fail(`a session's message has the correlation id ${show(entry.correlationId)}: ${show(values)}`);
    if (this.body === null) this.body = body;
    const payload = body.toString("latin1");
    if (!this.handed.has(payload)) this.handed.set(payload, []);
    const handed = this.handed.get(payload);
    handed.push(entry.position);
    if (this.noActionOn === payload && handed.length === 1) return 3; // Aeron's COMMIT, which Cavalry has no action for
    if (this.promiseOn === payload && handed.length === 1) return Promise.resolve(cavalry.CONTINUE);
    if (this.trueOn === payload && handed.length === 1) return true;
    if (this.edgesOn === payload && this.edges === null) {
      const tooLong = service.reply(entry.sessionId, Buffer.alloc(1345));
      this.edges = [[tooLong, addon.cavalry_errcode()], [service.reply(entry.sessionId + 1000, Buffer.from("x")), null]];
    }
    // Reply first, then change the state: an entry that is aborted is handed over again.
    const code = service.reply(entry.sessionId, Buffer.from(`${this.member}:${payload}`, "latin1"));
    if (code < 0) fail(`a reply from inside the handler answered ${code} with error ${addon.cavalry_errcode()}`);
    if (code === constants.CAVALRY_CLUSTER_REPLY_RETRY) {
      handed.pop();
      return cavalry.ABORT;
    }
    if (!this.codes.has(payload)) this.codes.set(payload, []);
    if (this.throwOn === payload && handed.length === 1) {
      this.codes.get(payload).push(code);
      throw new RangeError("the handler's own");
    }
    if (this.refusals === null) {
      const client = this.client;
      this.refusals = refusalsOf([
        ["poll", () => service.poll(this.onEntry, 1)], ["position", () => service.position()],
        ["answers", () => service.answers()], ["sessions", () => service.sessions()], ["fault", () => service.fault()],
        ["close", () => service.close()], ["client.isClosed", () => client.isClosed],
        ["client.clusterService", () => client.clusterService()],
        ["client.addSubscription", () => client.addSubscription("cavalry:ipc", 1229)]]);
    }
    if (payload === TIMER_MESSAGE) {
      // The program's state changes at the entry it takes. One timer, scheduled for an hour
      // later and then again for a little later: were the second schedule not to set the one
      // timer's deadline, the round's wait would end before the timer came. A second timer,
      // due at once, cancelled again: were the cancel to miss, it would be handed over. And
      // the least and the greatest int64 as ids, and the least and the greatest integer a
      // number carries exactly, due at once, which each service must be handed back as they
      // are. An id and a deadline go as a number here and as a BigInt there, since the addon
      // reads both. Every member's service makes the same calls at
      // the same entry.
      // Before them, an id with a fraction, which is refused here as it is outside the
      // handler, and schedules nothing.
      try {
        this.refusedInside = service.scheduleTimer(1.5, entry.timestamp);
      } catch (refused) {
        this.refusedInside = refused;
      }
      this.timerCodes = [
        service.scheduleTimer(TIMER_ID, entry.timestamp + HOUR_MS),
        service.scheduleTimer(BigInt(TIMER_ID), entry.timestamp + TIMER_AFTER_MS),
        service.scheduleTimer(CANCELLED_ID, entry.timestamp),
        service.cancelTimer(BigInt(CANCELLED_ID)),
        service.scheduleTimer(INT64_MIN, entry.timestamp),
        service.scheduleTimer(INT64_MAX, BigInt(entry.timestamp)),
        // The bounds of what a number carries exactly, as numbers: the addon takes both.
        service.scheduleTimer(-SAFE, entry.timestamp),
        service.scheduleTimer(SAFE, entry.timestamp)];
    }
    this.codes.get(payload).push(code);
    if (!this.messages.has(payload)) this.messages.set(payload, []);
    this.messages.get(payload).push(values);
    this.entries.push(values);
    return this.breakOn.includes(payload) ? cavalry.BREAK : cavalry.CONTINUE;
  }

  /** One poll, timed: the count of entries taken. A handler's failure is the caller's to
   * catch. */
  poll(limit = 16) {
    const before = this.entries.length;
    const started = process.hrtime.bigint();
    let count;
    try {
      count = this.service.poll(this.onEntry, limit);
    } finally {
      const took = process.hrtime.bigint() - started;
      this.polls += 1;
      if (took > 1000000n) this.overAMillisecond += 1;
      if (took > this.longest[0]) {
        this.longest = [took, `a poll of the service beside member ${this.member} that began after ${before} entries ` +
          `and took ${this.entries.length - before}`];
      }
    }
    if (count < 0) fail(`the poll of the service beside member ${this.member} stopped on fault ${show(this.service.fault())}`);
    return count;
  }
}

/** What the session's handler was handed. */
class Answered {
  constructor(session) {
    this.session = session;
    this.replies = []; // [payload, leadership term id, timestamp]
    this.refusals = null;
    this.throwOn = null; // a reply on which the handler throws
    this.values = null; // the object of values the handler was handed, the same one for every reply
    this.body = null; // the body of the first reply, kept past its call
    this.onReply = this.onReply.bind(this);
  }

  onReply(body, reply) {
    if (this.values === null) { this.values = reply; this.body = body; }
    if (reply !== this.values) fail("the handler was handed another object of values than the one the session keeps");
    const payload = body.toString("latin1");
    this.replies.push([payload, reply.leadershipTermId, reply.timestamp]);
    if (this.refusals === null) {
      const session = this.session;
      this.refusals = refusalsOf([
        ["offer", () => session.offer(Buffer.from("x"))], ["poll", () => session.poll(this.onReply, 1)],
        ["leadershipTermId", () => session.leadershipTermId()], ["leaderMemberId", () => session.leaderMemberId()],
        ["staleReplies", () => session.staleReplies()], ["closed", () => session.closed()],
        ["fault", () => session.fault()], ["close", () => session.close()]]);
    }
    if (this.throwOn !== null && payload.endsWith(this.throwOn)) throw new RangeError("the reply handler's own");
  }

  /** The replies to `message`, whichever member's service sent them. */
  to(message) {
    return this.replies.filter((reply) => reply[0].slice(2) === message);
  }
}

/** One duty cycle: every service's poll but `skip`'s, the session's when there is one, and a
 * short sleep. */
function cycle(served, answered = null, skip = null) {
  for (const s of served) {
    if (s !== skip) s.poll();
  }
  if (answered !== null && answered.session.poll(answered.onReply, 16) < 0) {
    fail(`the session's poll stopped on fault ${show(answered.session.fault())}`);
  }
  sleep(IDLE_MS);
}

function wait(what, done, served, answered = null, skip = null) {
  const until = Date.now() + WAIT_MS;
  while (!done()) {
    if (Date.now() > until) {
      fail(`${what}: not within ${WAIT_MS / 1000} s; the services took ${show(served.map((s) => s.entries.length))} ` +
        `entries and the session read ${answered !== null ? answered.replies.length : "no"} replies`);
    }
    cycle(served, answered, skip);
  }
}

function settle(served, answered = null, skip = null) {
  const until = Date.now() + SETTLE_MS;
  while (Date.now() < until) cycle(served, answered, skip);
}

function offer(session, message, served, answered) {
  const until = Date.now() + WAIT_MS;
  for (;;) {
    const offered = session.offer(Buffer.from(message, "latin1"));
    if (offered >= 0) return;
    if (offered === cavalry.ERROR) fail(`the offer of ${show(message)} failed with error ${addon.cavalry_errcode()}`);
    if (offered === cavalry.CLOSED) fail(`the session closed before ${show(message)}: ${session.closed()}`);
    if (Date.now() > until) {
      fail(`the session's publication did not take ${show(message)} within ${WAIT_MS / 1000} s: ${offered}`);
    }
    cycle(served, answered);
  }
}

/** The `HandlerError` of the leader's service's next poll that throws one, with the other
 * services and the session polled meanwhile. */
function handlerError(leader, served, answered, what) {
  const until = Date.now() + WAIT_MS;
  for (;;) {
    try {
      leader.poll();
    } catch (thrown) {
      if (!(thrown instanceof cavalry.HandlerError)) throw thrown;
      return thrown;
    }
    if (Date.now() > until) fail(`${what}: no poll of the leader's service threw HandlerError within ${WAIT_MS / 1000} s`);
    cycle(served, answered, leader);
  }
}

/** An entry whose first handling returned what is no action: the `HandlerError` its poll
 * threw, checked, and the entry handed over again at the same position and answered once. */
function noAction(leader, served, answered, session, message, returned) {
  offer(session, message, served, answered);
  const thrown = handlerError(leader, served, answered, `a handler that returns ${returned}`);
  const cause = thrown.cause;
  if (thrown.cluster !== "service" || thrown.raised !== 1 || thrown.undelivered !== 0 ||
      !(cause instanceof cavalry.CavalryError) || cause.code !== constants.CAVALRY_ERR_CLUSTER_ACTION ||
      !cause.abiMessage.includes(`returned ${returned}, which is no action`)) {
    fail(`a handler that returned ${returned} threw ${thrown} from ${cause}, with cluster ${show(thrown.cluster)}, ` +
      `raised ${thrown.raised} and undelivered ${thrown.undelivered}`);
  }
  wait(`the entry whose handler returned ${returned} is handed over again and answered`,
    () => answered.to(message).length > 0 && served.every((s) => s.messages.has(message)), served, answered);
  settle(served, answered);
  const handed = leader.handed.get(message);
  if (handed.length !== 2 || handed[0] !== handed[1] || answered.to(message).length !== 1) {
    fail(`the entry whose handler returned ${returned} was handed over at ${show(handed)} and the session read ` +
      `${show(answered.to(message))}`);
  }
}

function main() {
  if (process.argv.length !== 5) fail("usage: cluster.js <cluster directory> <members string> <response channel>");
  const [cluster, members, responseChannel] = process.argv.slice(2);

  // 1. A service beside each member, over a client on that member's driver.
  const served = [];
  for (let member = 0; member < MEMBERS; member++) served.push(new Served(cluster, member));
  // The handler's reply is for the handler: from here there is no entry.
  if (served[0].service.reply(1, Buffer.from("x")) !== -1 ||
      addon.cavalry_errcode() !== constants.CAVALRY_ERR_CLUSTER_NO_ENTRY) {
    fail(`a reply from outside the handler was not refused with CAVALRY_ERR_CLUSTER_NO_ENTRY: error ` +
      `${addon.cavalry_errcode()}`);
  }

  // 2. Each service opens its member's log and reads the term's first entry before the
  // session connects: a service answers no entry that its member held when the service first
  // opened the log (spec/cluster.md, "Replies").
  wait("each service reads an entry of its member's log", () => served.every((s) => s.entries.length > 0), served);

  // 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns once
  // the leader has opened the session.
  const client = new cavalry.Client();
  const session = client.connectCluster(members, responseChannel);
  const answered = new Answered(session);
  if (session.closed() !== null || !(session.sessionId >= 0) || !(session.maxMessageLength > 0)) {
    fail(`a session just opened reads closed ${show(session.closed())}, id ${session.sessionId} and a longest ` +
      `message of ${session.maxMessageLength} bytes`);
  }
  // What `offer` refuses: anything that is not bytes, before the ABI is called, and a message
  // longer than one frame carries.
  let refused = null;
  try {
    session.offer("hello");
  } catch (thrown) {
    refused = thrown;
  }
  if (!(refused instanceof TypeError)) fail(`the offer of a string was not refused with a TypeError: ${refused}`);
  if (session.offer(Buffer.alloc(session.maxMessageLength + 1)) !== cavalry.ERROR ||
      addon.cavalry_errcode() !== constants.CAVALRY_ERR_TOO_LONG) {
    fail(`a message of ${session.maxMessageLength + 1} bytes was not refused with ERROR and CAVALRY_ERR_TOO_LONG: ` +
      `error ${addon.cavalry_errcode()}`);
  }

  // 4. One message, taken by every service and answered by one.
  offer(session, "hello", served, answered);
  wait("the message is taken by three services and answered",
    () => answered.replies.length > 0 && served.every((s) => s.messages.has("hello")), served, answered);
  settle(served, answered);
  if (answered.replies.length !== 1) {
    fail(`the session read ${answered.replies.length} replies to one message: ${show(answered.replies)}`);
  }

  // 5. The same entry beside every member, answered by the leader's service alone.
  let leader = null;
  const first = served[0].messages.get("hello")[0];
  for (const s of served) {
    if (!same(s.messages.get("hello"), [first]) || first[4] !== session.sessionId) {
      fail(`the service beside member ${s.member} took the message as ${show(s.messages.get("hello"))}, and the ` +
        `service beside member 0 as ${show(first)}, of session ${session.sessionId}`);
    }
    if (s.service.sessions() !== 1) {
      fail(`the service beside member ${s.member} holds ${s.service.sessions()} sessions open, not one`);
    }
    if (!(s.service.position() > first[1])) {
      fail(`the service beside member ${s.member} is at ${s.service.position()}, not past the entry at ${first[1]}`);
    }
    if (s.service.fault().code !== 0) {
      fail(`the service beside member ${s.member} reads fault ${show(s.service.fault())} after polls that took entries`);
    }
    const expected = [["poll", -1, POLLING], ["position", -1, POLLING], ["answers", false, POLLING],
      ["sessions", -1, POLLING], ["fault", -1, POLLING], ["close", -1, POLLING], ["client.isClosed", false, POLLING],
      ["client.clusterService", -1, POLLING], ["client.addSubscription", -1, POLLING]];
    if (!same(s.refusals, expected)) {
      fail(`the calls from inside the handler of the service beside member ${s.member} answered ` +
        `${show(s.refusals)}, not ${show(expected)}`);
    }
    // The one object of values is the call's: outside it every getter is undefined, and the
    // body kept from a call reads as empty.
    if (s.values.position !== undefined || s.values.type !== undefined || s.values.timedOut !== undefined ||
        s.body.length !== 0) {
      fail(`outside the handler's call the entry's values read position ${s.values.position}, type ${s.values.type} ` +
        `and timed out ${s.values.timedOut}, and a kept body has ${s.body.length} bytes`);
    }
    if (same(s.codes.get("hello"), [SENT])) {
      if (leader !== null) fail(`the services beside members ${leader.member} and ${s.member} both answered one entry`);
      if (!s.service.answers()) fail(`the service beside member ${s.member} answered and says it does not answer`);
      leader = s;
    } else if (!same(s.codes.get("hello"), [constants.CAVALRY_CLUSTER_REPLY_NOT_ANSWERING]) || s.service.answers()) {
      fail(`the service beside member ${s.member}, a follower, answered ${show(s.codes.get("hello"))} and says it ` +
        `answers: ${s.service.answers()}`);
    }
  }
  if (leader === null) fail("no service answered the entry");
  if (!same(answered.replies, [[`${leader.member}:hello`, first[2], first[3]]])) {
    fail(`the reply ${show(answered.replies)} is not the leader's service's, of member ${leader.member}, with the ` +
      `entry's term ${first[2]} and the cluster's time ${first[3]}`);
  }
  const expected = [["offer", cavalry.ERROR, POLLING], ["poll", -1, POLLING], ["leadershipTermId", -1, POLLING],
    ["leaderMemberId", -1, POLLING], ["staleReplies", -1, POLLING], ["closed", null, POLLING], ["fault", -1, POLLING],
    ["close", -1, POLLING]];
  if (!same(answered.refusals, expected)) {
    fail(`the calls from inside the session's handler answered ${show(answered.refusals)}, not ${show(expected)}`);
  }
  if (answered.values.leadershipTermId !== undefined || answered.values.timestamp !== undefined ||
      answered.body.length !== 0) {
    fail(`outside the handler's call the reply's values read term ${answered.values.leadershipTermId} and time ` +
      `${answered.values.timestamp}, and a kept body has ${answered.body.length} bytes`);
  }
  if (session.leaderMemberId() !== leader.member || session.leadershipTermId() !== first[2] ||
      session.closed() !== null || session.fault().code !== 0) {
    fail(`the session sends to member ${session.leaderMemberId()} in term ${session.leadershipTermId()}, reads ` +
      `closed ${show(session.closed())} and fault ${show(session.fault())}; the entry's leader is member ` +
      `${leader.member} and its term ${first[2]}`);
  }
  const terms = leader.entries.filter((e) => e[0] === constants.CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM);
  const opens = leader.entries.filter((e) => e[0] === constants.CAVALRY_CLUSTER_ENTRY_SESSION_OPEN);
  if (terms.length === 0 || terms[terms.length - 1][5] !== leader.member ||
      !same(opens.map((e) => e[4]), [session.sessionId])) {
    fail(`the leader's service read the new leadership term events ${show(terms)} and the session open events ` +
      `${show(opens)}; the leader is member ${leader.member} and the session ${session.sessionId}`);
  }
  say(`session ${session.sessionId}'s message, the entry at position ${first[1]} of term ${first[2]}, taken by three ` +
    `services and answered once, by the service beside member ${leader.member}; nine calls from inside a service's ` +
    `handler and eight from inside the session's refused with CAVALRY_ERR_POLLING; the values a handler is handed ` +
    `read undefined after its call`);

  // 6. A handler that replies and then throws: its entry is aborted, so the poll ends at it
  // with the entry not taken, `poll` throws, and the next poll hands the entry over again; the
  // reply the first handling sent is not sent again.
  leader.throwOn = "throw";
  const before = leader.service.position();
  offer(session, "throw", served, answered);
  let thrown = handlerError(leader, served, answered, "a handler that throws");
  if (thrown.cluster !== "service" || thrown.raised !== 1 || thrown.undelivered !== 0 ||
      !(thrown.cause instanceof RangeError) || !(thrown.fragments >= 0) || thrown.committedLog !== false ||
      !thrown.message.includes("Its entry was aborted")) {
    fail(`a service's HandlerError has cluster ${show(thrown.cluster)}, fragments ${thrown.fragments}, raised ` +
      `${thrown.raised}, undelivered ${thrown.undelivered} and cause ${thrown.cause}, and says: ${thrown.message}`);
  }
  const aborted = leader.handed.get("throw")[0];
  if (!(before <= leader.service.position() && leader.service.position() <= aborted) || leader.messages.has("throw")) {
    fail(`after its handler threw on the entry at ${aborted} the service is at ${leader.service.position()}, from ` +
      `${before}, and took ${show(leader.messages.get("throw") || [])}`);
  }
  wait("the aborted entry is handed over again and its one reply read",
    () => answered.to("throw").length > 0 && served.every((s) => s.messages.has("throw")), served, answered);
  settle(served, answered);
  if (!same(leader.handed.get("throw"), [aborted, aborted]) || !same(leader.codes.get("throw"), [SENT, SENT]) ||
      answered.to("throw").length !== 1) {
    fail(`the entry whose handler threw was handed over at ${show(leader.handed.get("throw"))}, its replies answered ` +
      `${show(leader.codes.get("throw"))}, and the session read ${show(answered.to("throw"))}`);
  }

  // 7. A handler that returns what is no action, as a throw: aborted, thrown by `poll` with
  // the ABI's code, and handed over again. First 3, a number the ABI refuses; on its second
  // handling, before the reply, a reply too long for a frame and a reply to a session that is
  // not open. Then a promise, what an `async` handler returns, which the addon refuses without
  // a call: the handler has not finished with its entry. Then `true`, which a call-back
  // returns to say that it handled its entry: a boolean is no action, and the addon makes no
  // call with it, where 1, the number it would be as an integer, is CAVALRY_ACTION_ABORT.
  leader.noActionOn = leader.edgesOn = "no action";
  noAction(leader, served, answered, session, "no action", "3");
  const edges = [[constants.CAVALRY_CLUSTER_REPLY_TOO_LONG, constants.CAVALRY_ERR_TOO_LONG],
    [constants.CAVALRY_CLUSTER_REPLY_NO_SESSION, null]];
  if (!same(leader.edges, edges) || !same(leader.codes.get("no action"), [SENT])) {
    fail(`a reply of 1,345 bytes and a reply to session ${session.sessionId + 1000} answered ${show(leader.edges)}, ` +
      `not ${show(edges)}, and the reply after them ${show(leader.codes.get("no action"))}`);
  }
  leader.promiseOn = "promise";
  noAction(leader, served, answered, session, "promise", "[object Promise]");
  leader.trueOn = "true";
  noAction(leader, served, answered, session, "true", "true");

  // 8. BREAK: with two entries waiting, a poll takes one and ends. The leader's service is
  // not polled until the two others have taken both entries and a moment more, so that its
  // member has committed both.
  leader.breakOn = ["break 1", "break 2"];
  offer(session, "break 1", served, answered);
  offer(session, "break 2", served, answered);
  wait("the followers' services take two more entries",
    () => served.every((s) => s === leader || s.messages.has("break 2")), served, answered, leader);
  settle(served, answered, leader);
  const counts = [];
  let until = Date.now() + WAIT_MS;
  while (!leader.messages.has("break 2")) {
    if (Date.now() > until) fail(`the leader's service took ${show(counts)} entries in its polls and not both entries`);
    const count = leader.poll();
    if (count !== 0) counts.push(count);
    else cycle(served, answered, leader);
  }
  if (!same(counts, [1, 1])) {
    fail(`with BREAK returned for each of two entries the leader's polls took ${show(counts)}, not one each`);
  }
  wait("the two replies are read", () => answered.to("break 1").length > 0 && answered.to("break 2").length > 0,
    served, answered);
  if (!same(answered.replies.slice(-2).map((reply) => reply[0].slice(2)), ["break 1", "break 2"])) {
    fail(`the replies to two messages came out of order: ${show(answered.replies.slice(-2))}`);
  }

  // 9. The session's handler throws: the reply was handed over, `poll` throws once the ABI
  // has returned, and the reply is not handed over again.
  answered.throwOn = "reply throws";
  offer(session, "reply throws", served, answered);
  thrown = null;
  until = Date.now() + WAIT_MS;
  while (thrown === null) {
    if (Date.now() > until) fail("the reply on which the session's handler throws was not read");
    try {
      cycle(served, answered);
    } catch (error) {
      if (!(error instanceof cavalry.HandlerError)) throw error;
      thrown = error;
    }
  }
  answered.throwOn = null;
  if (thrown.cluster !== "session" || thrown.fragments !== 1 || thrown.raised !== 1 || thrown.undelivered !== 0 ||
      !(thrown.cause instanceof RangeError) || !thrown.message.includes("the reply handler threw on 1 of the 1 reply")) {
    fail(`a session's HandlerError has cluster ${show(thrown.cluster)}, fragments ${thrown.fragments}, raised ` +
      `${thrown.raised}, undelivered ${thrown.undelivered} and cause ${thrown.cause}, and says: ${thrown.message}`);
  }
  settle(served, answered);
  if (answered.to("reply throws").length !== 1 || session.staleReplies() !== 0) {
    fail(`the reply whose handler threw was read ${answered.to("reply throws").length} times, and the session passed ` +
      `over ${session.staleReplies()} stale replies`);
  }
  say("a handler that threw, one that returned 3, one that returned a promise and one that returned true had their entries aborted, handed " +
    "over again and answered once; a reply of 1,345 bytes and a reply to no session refused; BREAK took one of two " +
    "entries; a reply whose handler threw is not handed over again");

  // 9b. The program's timers (spec/c-abi.md, version 7; spec/cluster.md, "The service",
  // "Timers"; ADR 0157). Each service's program schedules six timers at one message and
  // cancels one: the five are handed to every service once each, as entries of their own
  // type with the timers' ids, at one position each and none earlier than its deadline, and
  // the cancelled one to none. Step 10 holds the "once" for a timer refresh interval and
  // more. An argument that is no int64 never reaches the ABI: the thread's error is as it
  // was set.
  for (const [what, call, error] of [
    ["an id with a fraction", () => served[0].service.scheduleTimer(1.5, 1), RangeError],
    ["an id past the safe integers", () => served[0].service.scheduleTimer(2 ** 53, 1), RangeError],
    ["an id below the safe integers", () => served[0].service.scheduleTimer(-(2 ** 53), 1), RangeError],
    ["a deadline that is no number", () => served[0].service.scheduleTimer(1, NaN), RangeError],
    ["an infinite deadline", () => served[0].service.scheduleTimer(1, Infinity), RangeError],
    ["a deadline of minus infinity", () => served[0].service.scheduleTimer(1, -Infinity), RangeError],
    ["a deadline below every int64", () => served[0].service.scheduleTimer(1, -1e300), RangeError],
    ["a boolean as an id", () => served[0].service.scheduleTimer(true, 1), TypeError],
    ["no deadline", () => served[0].service.scheduleTimer(1, undefined), TypeError],
    ["a BigInt above int64 as an id", () => served[0].service.scheduleTimer(INT64_MAX + 1n, 1), RangeError],
    ["a BigInt below int64 as a deadline", () => served[0].service.scheduleTimer(1, INT64_MIN - 1n), RangeError],
    ["a string as an id", () => served[0].service.scheduleTimer("7", 1), TypeError],
    ["an id with a fraction to cancel", () => served[0].service.cancelTimer(7.25), RangeError]]) {
    setAnotherError();
    const before = addon.cavalry_errcode();
    let answeredWith;
    try {
      answeredWith = call();
    } catch (thrown) {
      answeredWith = thrown;
    }
    if (!(answeredWith instanceof error) || addon.cavalry_errcode() !== before) {
      fail(`${what} was not refused with ${error.name} before the ABI was called: it answered ${answeredWith}, and ` +
        `the thread's error went from ${before} to ${addon.cavalry_errcode()}`);
    }
    if (!answeredWith.message.includes("clients/node/README.md#a-services-timers")) {
      fail(`the refusal of ${what} does not name the README's section: ${answeredWith.message}`);
    }
  }
  for (const [name, call] of [["scheduleTimer", () => served[0].service.scheduleTimer(1, 1)],
    ["cancelTimer", () => served[0].service.cancelTimer(1n)]]) {
    setAnotherError();
    if (call() !== -1 || addon.cavalry_errcode() !== constants.CAVALRY_ERR_CLUSTER_NO_ENTRY) {
      fail(`${name} from outside the handler was not refused with CAVALRY_ERR_CLUSTER_NO_ENTRY: error ` +
        `${addon.cavalry_errcode()}`);
    }
  }
  offer(session, TIMER_MESSAGE, served, answered);
  const timerIds = [INT64_MIN, BigInt(-SAFE), BigInt(TIMER_ID), BigInt(SAFE), INT64_MAX];
  wait("each service is handed its five timers", () => served.every((s) => s.timers.length >= timerIds.length),
    served, answered);
  settle(served, answered);
  const scheduled = served[0].messages.get(TIMER_MESSAGE)[0];
  for (const s of served) {
    if (!same(s.timerCodes, [SCHEDULED, SCHEDULED, SCHEDULED, 0, SCHEDULED, SCHEDULED, SCHEDULED, SCHEDULED])) {
      fail(`the service beside member ${s.member} was answered ${show(s.timerCodes)} for its schedules and its ` +
        `cancel; expected CAVALRY_CLUSTER_TIMER_SCHEDULED, ${SCHEDULED}, for each schedule and 0 for the cancel`);
    }
    if (!(s.refusedInside instanceof RangeError)) {
      fail(`the service beside member ${s.member} scheduled an id with a fraction from inside its handler and was ` +
        `answered ${s.refusedInside}; expected a RangeError before the ABI was called`);
    }
    const ids = s.timers.map((t) => t[1]).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    if (!same(ids, timerIds) || s.timers.some((t) => typeof t[1] !== "bigint")) {
      fail(`the service beside member ${s.member} was handed the timers ${show(s.timers)}, each a position, an id ` +
        `and a time; expected timer ${TIMER_ID}, the least int64 and the greatest and the least safe integer and ` +
        `the greatest once each, each id a BigInt, and timer ${CANCELLED_ID}, which it cancelled, never`);
    }
    if (!same(s.timers, served[0].timers)) {
      fail(`the service beside member ${s.member} was handed its timers as ${show(s.timers)} and the service beside ` +
        `member ${served[0].member} as ${show(served[0].timers)}; expected one position and one time for each`);
    }
    for (const [position, correlationId, timestamp] of s.timers) {
      const after = correlationId === BigInt(TIMER_ID) ? TIMER_AFTER_MS : 0;
      if (timestamp < scheduled[3] + after || position <= scheduled[1]) {
        fail(`the service beside member ${s.member} was handed timer ${correlationId} at position ${position} and ` +
          `time ${timestamp}; it was scheduled at position ${scheduled[1]} and time ${scheduled[3]} for ${after} ms ` +
          `later`);
      }
    }
  }
  const timer = served[0].timers.find((t) => t[1] === BigInt(TIMER_ID));
  // What a deadline that was ignored would have read: the time of the timers that were due
  // at once, which the member was told of with timer 77. The deadline check above proves
  // something for as long as this is below the deadline.
  const atOnce = Math.max(...served[0].timers.filter((t) => t[1] !== BigInt(TIMER_ID)).map((t) => t[2])) - scheduled[3];
  const margin = atOnce < TIMER_AFTER_MS ? "" : ", so the deadline's check proved nothing in this run";
  say(`timer ${TIMER_ID}, scheduled by each service at a message for an hour later and then for ${TIMER_AFTER_MS} ms ` +
    `later, handed to three services once, at position ${timer[0]}, ${timer[2] - scheduled[3]} ms after the ` +
    `message; the least and the greatest int64 and the least and the greatest safe integer as ids, due at once, ` +
    `handed to each once with those ids, ${atOnce} ms after the message${margin}; timer ${CANCELLED_ID}, due at ` +
    `once and cancelled in the same handling, handed to none; both calls refused from outside the handler, and ` +
    `what is no int64 before the ABI is called, from inside the handler too`);

  // 10. The session before its client. Its close is an entry every service is handed, after
  // which the log holds no session open.
  session.close();
  session.close(); // a second close does nothing
  if (session.closed() !== null) fail(`a session that close() freed reads closed ${show(session.closed())}`);
  client.close();
  const closes = (s) => s.entries.filter((e) => e[0] === constants.CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE);
  wait("each service reads the session's close", () => served.every((s) => closes(s).length > 0), served);
  for (const s of served) {
    if (!same(closes(s).map((e) => [e[4], e[6]]), [[first[4], false]]) || s.service.sessions() !== 0) {
      fail(`the service beside member ${s.member} read the session close events ${show(closes(s))} and holds ` +
        `${s.service.sessions()} sessions open; the session ${first[4]} was closed by its client`);
    }
  }
  if (served.some((s) => !same(s.entries, served[0].entries))) {
    fail(`the three services took different entries: ${show(served.map((s) => s.entries))}`);
  }
  // The timers of step 9b, once each: a leader gives a timer that is still pending in its
  // service another event a timer refresh interval after the first, so the services are
  // polled until that long has passed since the last of them was handed a timer.
  const refreshed = Math.max(...served.map((s) => s.timerAt)) + REFRESH_MS;
  while (Date.now() < refreshed) cycle(served);
  for (const s of served) {
    if (s.timers.length !== 5) {
      fail(`the service beside member ${s.member} was handed the timers ${show(s.timers)} by ${REFRESH_MS} ms after ` +
        `the last; expected five, once each`);
    }
  }

  // 11. Each service before its own client, twice; and the last after its client.
  for (const s of served.slice(0, -1)) {
    s.service.close();
    s.service.close(); // a second close does nothing
    s.client.close();
  }
  served[served.length - 1].client.close();
  served[served.length - 1].service.close();
  const longest = served.map((s) => s.longest).reduce((a, b) => (b[0] > a[0] ? b : a));
  const polls = served.reduce((sum, s) => sum + s.polls, 0);
  const over = served.reduce((sum, s) => sum + s.overAMillisecond, 0);
  say(`${served[0].entries.length} entries taken by each of three services; the session's close read by each; a ` +
    `service closed after its client`);
  say(`report: ${polls.toLocaleString("en-US")} polls of the three services, each holding the thread for its run: ` +
    `${over} took over 1 ms, and the longest ${(longest[0] / 1000n).toLocaleString("en-US")} us, ${longest[1]} (not gated)`);

  // The Buffers the handlers were handed, one for each entry and each reply, an aborted
  // entry's each time it was handed over: Node frees their native objects when its event loop
  // turns, and the addon's finalizer counts each one back.
  const held = cavalry.buffersHeld();
  setImmediate(guarded(() => {
    if (cavalry.buffersHeld() !== 0) {
      fail(`${cavalry.buffersHeld()} of the ${held} Buffers the round's handlers were handed are still held after one ` +
        `turn of the event loop`);
    }
    say(`${held} Buffers handed to the round's handlers, all freed by one turn of the event loop`);
    process.stdout.write("conformance/node: cluster round ok\n");
  }));
}

/** `fn`, with what it throws reported as the round's failure. */
function guarded(fn) {
  return () => {
    try {
      fn();
    } catch (thrown) {
      if (thrown instanceof cavalry.CavalryError) fail(`${thrown.fn} failed with error ${thrown.code}: ${thrown.abiMessage}`);
      if (thrown instanceof cavalry.HandlerError) fail(`the handler threw: ${thrown.cause}`);
      throw thrown;
    }
  };
}

guarded(main)();
