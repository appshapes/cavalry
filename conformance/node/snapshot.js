#!/usr/bin/env node
/* The Node client's snapshot round (spec/c-abi.md, version 6, "A service's snapshot and its
 * start"; ADR 0146, ADR 0147, ADR 0149): a service beside each of three cluster members that
 * conformance/node/run.sh started with `cavalry run` takes a snapshot of its program's state
 * that its member lists, and a service that starts again starts from it.
 *
 *     snapshot.js <cluster directory> <the cavalry binary>
 *
 * The members' drivers are <cluster directory>/member-0, -1 and -2. The round runs after the
 * cluster round, on the log that round left. It is the Python runner's,
 * conformance/python/snapshot.py, through this client (conformance/README.md, "The snapshot
 * round"):
 *
 * 1. Each service asks where it starts, before its first poll: `restore()` answers
 *    CAVALRY_CLUSTER_RESTORED_FROM_START and no state, and a second is refused with
 *    CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE.
 * 2. Each service reads an entry of the log. No snapshot is due, and `snapshot()` is
 *    refused with CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE.
 * 3. `cavalry snapshot` is run beside each member: beside the leader it exits 0 and prints
 *    the snapshot's log position, and beside a follower it exits 1 and names the leader.
 * 4. Each service's poll ends at the leader's request, handed over as
 *    CAVALRY_CLUSTER_ENTRY_SNAPSHOT, and `snapshotDue()` is the position the command
 *    printed; the three services hold one state there. From inside the handler at the
 *    request, `snapshotDue()` is null with the thread's error at CAVALRY_ERR_POLLING, and
 *    `snapshot()` and `restore()` are refused with it.
 * 5. Each service gives its state: `snapshot(state)` is true. None is due after, and a
 *    second is refused. The three services are then closed: one service beside a member
 *    takes snapshots.
 * 6. Beside each member a new service starts from the snapshot: `restore()` answers
 *    CAVALRY_CLUSTER_RESTORED_STATE and the state as it was given, which only a member that
 *    lists the snapshot names, and its position is the snapshot's.
 * 7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
 *    the request, at the first snapshot's position and no entry below it; its poll ends
 *    there, and it gives its state. A start beside each member is then given that state.
 * 8. A service that never asks where it starts reads the log from position 0. It meets the
 *    first request with the state the first services gave, and the second with the state
 *    the restarted ones hold. Its `snapshot()` is false both times.
 *
 * The program's state is how many entries it has read and a hash of each entry's type,
 * position, term, time and payload, in order, written out 77 times, 2,472 bytes, so that a
 * snapshot holds it in three messages: the same beside every member. Every wait is bounded
 * at 60 s, and a snapshot that the driver gave up at a step is given again. It exits 1 with
 * what it found when a check fails. Node's standard library only (ADR 0003). */
"use strict";

const childProcess = require("child_process");
const crypto = require("crypto");
const path = require("path");

const cavalry = require(path.join(__dirname, "..", "..", "clients", "node"));
const { constants } = cavalry;
const addon = cavalry.addon;

const MEMBERS = 3;
const WAIT_MS = 60000;
const IDLE_MS = 1;      // between duty cycles: the members run in processes of their own
const STATE_LENGTH = 8 + 32 * 77;  // 2,472 bytes: three messages of a snapshot
const SNAPSHOT = constants.CAVALRY_CLUSTER_ENTRY_SNAPSHOT;
const FROM_START = constants.CAVALRY_CLUSTER_RESTORED_FROM_START;
const NO_MEMBER = constants.CAVALRY_CLUSTER_RESTORED_NO_MEMBER;
const STATE = constants.CAVALRY_CLUSTER_RESTORED_STATE;

function fail(what) {
  console.error(`conformance/node: snapshot round: ${what}`);
  process.exit(1);
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** What `call` returned, or the code of the `CavalryError` it threw. */
function codeOf(call) {
  try {
    return `returned ${JSON.stringify(call())}`;
  } catch (thrown) {
    if (!(thrown instanceof cavalry.CavalryError)) throw thrown;
    return `threw ${thrown.code}`;
  }
}

/** A service beside one member and its program: the state is how many entries it has read
 * and a hash over them, which every member's service computes alike. */
class Program {
  constructor(cluster, member) {
    this.member = member;
    this.client = new cavalry.Client(path.join(cluster, `member-${member}`));
    this.service = this.client.clusterService();
    this.count = 0;
    this.digest = Buffer.alloc(32);
    this.entries = [];     // the type and the position of every entry taken since it began
    this.inside = null;    // what the three calls answered from inside the handler
    this.onEntry = (body, entry, service) => {
      const values = Buffer.alloc(28);
      values.writeInt32LE(entry.type, 0);
      values.writeBigInt64LE(BigInt(entry.position), 4);
      values.writeBigInt64LE(BigInt(entry.leadershipTermId), 12);
      values.writeBigInt64LE(BigInt(entry.timestamp), 20);
      this.count += 1;
      this.digest = crypto.createHash("sha256").update(this.digest).update(values).update(body).digest();
      this.entries.push(`${entry.type}@${entry.position}`);
      if (entry.type === SNAPSHOT && this.inside === null) {
        // The poll holds the service and its client. The read answers null for a refusal
        // as for "none due", so the thread's error is set to another code first, as the
        // cluster round does, and must be CAVALRY_ERR_POLLING after.
        if (addon.cavalry_committed_log_entry_position(null) !== -1) fail("the position of a null header did not answer -1");
        const due = service.snapshotDue();
        this.inside = [
          `${due}`,
          `${addon.cavalry_errcode()}`,
          codeOf(() => service.snapshot(this.state())),
          codeOf(() => service.restore()),
        ].join(", ");
      }
    };
  }

  /** The program's state as bytes: the count, eight bytes, and the hash 77 times. */
  state() {
    const count = Buffer.alloc(8);
    count.writeBigInt64LE(BigInt(this.count), 0);
    return Buffer.concat([count, ...Array(77).fill(this.digest)]);
  }

  /** Starts the program from `state`, as its snapshot held it. */
  take(state) {
    if (state.length !== STATE_LENGTH) fail(`a state of ${state.length} bytes that is not one the program gave`);
    this.count = Number(state.readBigInt64LE(0));
    this.digest = Buffer.from(state.subarray(8, 40));
  }

  /** Gives the program's state to the service's snapshot: what it answered. A snapshot
   * that a step of the driver's gave up is still due, and is given again. */
  give() {
    const deadline = Date.now() + WAIT_MS;
    for (;;) {
      const answered = codeOf(() => this.service.snapshot(this.state()));
      if (answered !== `threw ${constants.CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED}` || Date.now() > deadline) return answered;
      sleep(10);
    }
  }

  poll() {
    const count = this.service.poll(this.onEntry, 16);
    if (count < 0) {
      fail(`the poll of the service beside member ${this.member} stopped on fault ${JSON.stringify(this.service.fault())}`);
    }
    return count;
  }

  /** Asks where the service starts until its driver hosts a member with a log. */
  restore() {
    const deadline = Date.now() + WAIT_MS;
    for (;;) {
      const found = this.service.restore();
      if (found.restored !== NO_MEMBER) return found;
      if (Date.now() > deadline) fail(`the driver of member ${this.member} hosts no member with a log after 60 s`);
      sleep(1);
    }
  }

  close() {
    this.service.close();
    this.client.close();
  }
}

/** Polls every program's service that has no snapshot to give until `done`. */
function wait(what, done, programs) {
  const deadline = Date.now() + WAIT_MS;
  while (!done()) {
    if (Date.now() > deadline) fail(`${what}, after 60 s`);
    for (const program of programs) {
      if (program.service.snapshotDue() === null) program.poll();
    }
    sleep(IDLE_MS);
  }
}

/** `cavalry snapshot` beside member `member`: its exit code and what it printed. */
function ask(binary, cluster, member) {
  const ran = childProcess.spawnSync(binary, ["snapshot", "--dir", path.join(cluster, `member-${member}`)],
    { encoding: "utf8", timeout: WAIT_MS });
  if (ran.error && ran.error.code !== "ETIMEDOUT") fail(`\`cavalry snapshot\` beside member ${member} did not run: ${ran.error.message}`);
  if (ran.status === null) fail(`\`cavalry snapshot\` beside member ${member} did not end in 60 s`);
  return ran;
}

/** The log position that `cavalry snapshot` printed beside member `member`. */
function positionOf(ran, member) {
  const prefix = `member ${member} lists a snapshot of log position `;
  if (ran.status !== 0 || !ran.stdout.startsWith(prefix)) {
    fail(`\`cavalry snapshot\` beside member ${member}, exit ${ran.status}: ${(ran.stdout + ran.stderr).trim()}`);
  }
  return Number(ran.stdout.slice(prefix.length).split(":")[0].split(",")[0]);
}

function main() {
  if (process.argv.length !== 4) fail("usage: snapshot.js <cluster directory> <the cavalry binary>");
  const [cluster, binary] = process.argv.slice(2);
  const tooLate = `threw ${constants.CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE}`;
  const noneDue = `threw ${constants.CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE}`;
  const polling = `threw ${constants.CAVALRY_ERR_POLLING}`;
  const members = [...Array(MEMBERS).keys()];

  // 1. Where each service starts: no snapshot of a service is listed yet.
  const programs = members.map((member) => new Program(cluster, member));
  for (const program of programs) {
    if (program.service.snapshotDue() !== null) {
      fail(`a snapshot is due beside member ${program.member} before its service's first poll`);
    }
    const found = program.restore();
    if (found.restored !== FROM_START || found.state !== null) {
      fail(`the service beside member ${program.member} starts with ${JSON.stringify(found)}, not from the log's start`);
    }
    const again = codeOf(() => program.service.restore());
    if (again !== tooLate) fail(`a second restore beside member ${program.member}: ${again}`);
  }

  // 2. Every service reads an entry of the log the cluster round left; none is due.
  wait("every service read an entry", () => programs.every((program) => program.count > 0), programs);
  for (const program of programs) {
    const refused = codeOf(() => program.service.snapshot(program.state()));
    if (refused !== noneDue) fail(`a snapshot where none is due beside member ${program.member}: ${refused}`);
  }

  // 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's names the
  // leader.
  const asked = members.map((member) => ask(binary, cluster, member));
  const leaders = members.filter((member) => asked[member].status === 0);
  if (leaders.length !== 1) {
    fail("`cavalry snapshot` did not exit 0 beside exactly one member: " +
      asked.map((ran, member) => `member ${member}: exit ${ran.status}: ${(ran.stdout || ran.stderr).trim()}`).join("; "));
  }
  const leader = leaders[0];
  const position = positionOf(asked[leader], leader);
  asked.forEach((ran, member) => {
    if (member !== leader && (ran.status !== 1 || !ran.stderr.includes(`member ${leader} leads`))) {
      fail(`\`cavalry snapshot\` beside member ${member}, a follower, exit ${ran.status}: ${ran.stderr.trim()}`);
    }
  });

  // 4. Every service's poll ends at the request, and the snapshot is due at the position
  // the command printed, with one state beside the three members.
  wait("every service's poll ended at the leader's request",
    () => programs.every((program) => program.service.snapshotDue() !== null), programs);
  for (const program of programs) {
    const due = program.service.snapshotDue();
    const last = program.entries[program.entries.length - 1];
    if (due !== position || program.service.position() !== position || !last.startsWith(`${SNAPSHOT}@`)) {
      fail(`beside member ${program.member} the snapshot is due at ${due}, the service is at ` +
        `${program.service.position()} and its last entry is ${last}: the command printed ${position}`);
    }
    if (program.inside !== `null, ${constants.CAVALRY_ERR_POLLING}, ${polling}, ${polling}`) {
      fail(`from inside the handler at the request, beside member ${program.member}: ${program.inside}`);
    }
  }
  const given = programs[0].state();
  if (given.length !== STATE_LENGTH || programs.some((program) => !program.state().equals(given))) {
    fail("the three services do not hold one state of 2,472 bytes at the snapshot's position");
  }

  // 5. Each service gives its state, once, and is closed: the service that starts from
  // the snapshot is the one that takes the next.
  for (const program of programs) {
    const written = program.give();
    if (written !== "returned true") fail(`the service beside member ${program.member} gave its state: ${written}`);
    if (program.service.snapshotDue() !== null) {
      fail(`a snapshot is still due beside member ${program.member} after it was taken`);
    }
    const again = codeOf(() => program.service.snapshot(program.state()));
    if (again !== noneDue) fail(`a second snapshot beside member ${program.member}: ${again}`);
    program.close();
  }

  // 6. The member lists it: a new service beside each member starts from it. A member lists
  // the snapshot when it reads the service's word of it, a duty cycle or so later, so a
  // start that finds none is made again with a new service.
  const restarted = [];
  for (const member of members) {
    const deadline = Date.now() + WAIT_MS;
    let program;
    let found;
    for (;;) {
      program = new Program(cluster, member);
      found = program.restore();
      if (found.restored === STATE) break;
      program.close();
      if (Date.now() > deadline) {
        fail(`member ${member} lists no snapshot of its service after 60 s: a new service starts with ${found.restored}`);
      }
      sleep(10);
    }
    if (!found.state.equals(given)) {
      fail(`the service beside member ${member} starts from ${found.state.length} bytes that are not the state given`);
    }
    program.take(found.state);
    if (program.service.position() !== position) {
      fail(`the service beside member ${member} starts at ${program.service.position()}, not at ${position}`);
    }
    const again = codeOf(() => program.service.restore());
    if (again !== tooLate) fail(`a second restore beside member ${member}: ${again}`);
    restarted.push(program);
  }

  // 7. A second request, beside the leader: each restarted service's first entry is the
  // request, at the first snapshot's position, and it gives its state.
  const second = positionOf(ask(binary, cluster, leader), leader);
  if (second <= position) fail(`a second snapshot at ${second}, not past the first at ${position}`);
  wait("every restarted service's poll ended at the second request",
    () => restarted.every((program) => program.service.snapshotDue() !== null), restarted);
  for (const program of restarted) {
    if (program.entries.join() !== `${SNAPSHOT}@${position}` || program.service.snapshotDue() !== second) {
      fail(`the service beside member ${program.member} that started from its snapshot at ${position} was handed ` +
        `${program.entries}, with a snapshot due at ${program.service.snapshotDue()}: the second request is the ` +
        `one entry at ${position}, and asks for a snapshot at ${second}`);
    }
  }
  const after = restarted[0].state();
  if (after.equals(given) || restarted.some((program) => !program.state().equals(after))) {
    fail("the restarted services do not hold one new state at the second snapshot's position");
  }
  for (const program of restarted) {
    const written = program.give();
    if (written !== "returned true") fail(`the restarted service beside member ${program.member} gave its state: ${written}`);
  }
  // Each member lists the second snapshot too: a start beside it is given the new state,
  // once the member has read the service's word of it.
  for (const member of members) {
    const deadline = Date.now() + WAIT_MS;
    for (;;) {
      const probe = new Program(cluster, member);
      const found = probe.restore();
      const at = probe.service.position();
      probe.close();
      if (found.restored === STATE && found.state.equals(after) && at === second) break;
      if (Date.now() > deadline) {
        fail(`member ${member} does not list its service's second snapshot after 60 s: a new service starts with ${found.restored} at ${at}`);
      }
      sleep(10);
    }
  }

  // 8. A service that never asks reads from position 0 and meets both requests, each with
  // the state the others held there: a start from a snapshot and a read from the start
  // reach one state. Its member lists a snapshot at each, or past it.
  const late = new Program(cluster, leader);
  for (const [at, held] of [[position, given], [second, after]]) {
    wait(`a service that reads from the start meets the request for ${at}`, () => late.service.snapshotDue() !== null, [late]);
    if (late.service.snapshotDue() !== at || !late.state().equals(held)) {
      fail(`a service that read from the start is due at ${late.service.snapshotDue()}, not at ${at}, or holds another state than the services held there`);
    }
    const listed = late.give();
    if (listed !== "returned false") fail(`a snapshot of position ${at}, which the member already lists or is past: ${listed}`);
  }

  for (const program of [...restarted, late]) program.close();
  console.log(`conformance/node: snapshot round: three services gave a state of ${given.length} bytes at log ` +
    `position ${position}, asked for by \`cavalry snapshot\` beside member ${leader}; each member listed its ` +
    `service's snapshot, a new service beside each started from it, read the next request at ${position} and gave its state at ${second}`);
  console.log("conformance/node: snapshot round ok");
}

main();
