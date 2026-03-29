# Nile Runtime
Nile Runtime is a hand crafted single-threaded async rust runtime built on top of mio, it features an executor, Reactor, Waker, Timers and OS driven wakers

### Architecture Overview
Tasks are assigned unique IDs and stored in the executor. When spawned, the task ID is pushed onto the ready queue. The executor polls ready tasks in a loop; when a task returns Pending, its waker is stored in the reactor's token→waker map, keyed by the mio token for that resource. When mio reports a token as ready, the reactor looks up the corresponding waker and wakes the task.

Wakers wake a task by sending its ID through a channel, which the executor drains once its ready queue is exhausted.
The reactor handle is passed to futures via thread_local!, allowing async operations to access it without explicit context passing. It exposes:

register — registers interest in an I/O resource with a token
the token→waker map — for storing and looking up wakers

Timers use a min-heap ordered by deadline. When the executor exhausts its ready queue, it sets mio::Poll::poll's timeout to the nearest deadline — so either an I/O event wakes a task early, or the timeout fires and the timer task is woken.


### Example
See [`bin/example.rs`](bin/example.rs) for a full example covering sleep, async TCP, and concurrent tasks.
```bash
cargo run --bin example
```
