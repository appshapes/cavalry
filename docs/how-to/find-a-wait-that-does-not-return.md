# Find a wait that does not return

A how-to page: one task, telling whether a program that hangs is held by a short
timed wait that never came back, and finding out whether a machine has that fault.
Why Cavalry's threads park where a stop can wake them is
[ADR 0171](../decisions/0171-park-the-idle-loops-where-a-stop-can-wake-them-and-wake-every-thread-at-a-stop.md).

The fault: a thread asks the system to wake it after at most a millisecond, with
`nanosleep` or a park with a timeout, and the wake-up of the timer never comes. The
thread is sound, and returns at once when anything else wakes it. It was found on
one machine, a Mac on macOS 26.6.1, where one such wait in about 20 million was
lost. It was not seen on Linux, or on macOS 26.3 in a virtual machine, in 300 million
waits each.

## Prerequisites

- The process that hangs, still running, and its process id.
- On macOS, `sample`, which the system has. On Linux, `gdb` or `eu-stack`.
- For the probe: `rustc`, and this repository's `scripts/sleep-probe.rs`.

## Steps

1. Take two samples of the process, some seconds apart:

   ```sh
   # ignore: needs the id of a process that hangs
   sample <pid> 1 -file first.txt
   sample <pid> 1 -file second.txt
   ```

   Each takes one second and writes every thread's stack with a count of how many of
   its samples held it. You will see `Sample analysis of process <pid> written to
   file first.txt`.

2. Find the thread that the program waits on. In a test that hangs it is most often
   the thread a join waits for: the test's own thread is in `std::thread::JoinHandle`
   `join`, below `Driver::stop` or `Cavalry::shutdown`, and the thread it joins is
   one of `cavalry-conductor`, `cavalry-recorder`, `cavalry-replayer`,
   `cavalry-sender`, `cavalry-receiver` or `cavalry-client-conductor`.

3. Read that thread's stack in both files.

   - **One wait that does not return.** Every sample of both files holds the same
     stack, ending in `nanosleep` and `__semwait_signal`, or in `park_timeout` and
     `semaphore_timedwait_trap`. A loop that turns shows other frames in some of its
     samples, as the same process's other threads do. This is the fault.
   - **A loop that turns.** The samples hold more than one stack. The thread is
     alive and its loop does not end: that is a bug of ours, in what the loop waits
     for, and not this page's.

4. Stop your own run, by the id of the task or the process you started, and run it
   again. A wait that does not return is the machine's and not the change's.

5. Find out whether a machine has the fault. Build the probe and run it for twenty
   minutes:

   ```sh
   # ignore: runs for twenty minutes
   rustc -O --edition 2021 scripts/sleep-probe.rs -o target/sleep-probe
   target/sleep-probe 1200 128 0
   ```

   The probe starts 128 threads that each repeat the backoff of Cavalry's idle
   loops, a wait of 1 µs doubling to 1 ms, in `std::thread::sleep`, in `nanosleep`
   and in `std::thread::park_timeout`. Its main thread prints `STUCK` for a thread
   whose one wait has lasted three seconds, wakes it after thirty, and prints
   `RETURNED` when it comes back. Each minute it prints the count of waits so far.
   The last line is `stuck sleeps seen: N; still stuck at the end: M`, and the
   probe ends with 0 when N is 0.

6. Record what you found where the team keeps it: the machine, the system's version
   (`sw_vers` and `uname -v` on macOS, `uname -a` on Linux), and the probe's last
   four lines. The machines read so far are in ADR 0171, "Evidence".

## How to verify it worked

- Step 3 put the thread in one of its two cases, from two files and not one: one
  sample cannot tell a wait that never returns from a loop that waits again.
- Step 5 ended with the line `stuck sleeps seen:`. With N at 0 after twenty minutes,
  about 130 million waits, the machine has not shown the fault; that is not proof
  that it has none.

## When it does not work

- **`sample` says the process does not exist or cannot be examined.** The run ended,
  or it is another user's. Take the samples while it hangs, as the user who started
  it.
- **The probe does not build on your system.** It calls `nanosleep`, `pthread_kill`
  and `signal` by their C names and reads `errno` as macOS and Linux give it. On
  another system, give it that system's name for `errno`.
- **A Cavalry program hangs in a join on a machine that has the fault, at a commit
  after ADR 0171.** A stop wakes every thread it joins, so a lost wake-up alone does
  not hold it. Read step 3 again: the thread it joins is in another call, or the
  program is one that ADR 0171 names as not covered, such as a test's own wait in
  `std::thread::sleep`.
- **A driver or a client does no work, and no stop was asked for.** A lost wake-up
  holds an idle thread until something wakes it, and ADR 0171 says which threads
  nothing wakes while they run. Step 1 and step 3 show it: the thread is in one park
  in every sample.
