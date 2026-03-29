use mio::{Events, Interest, Poll, Token};
use std::cell::RefCell;
use std::collections::{BinaryHeap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::task::{Wake, Waker};
use std::time::Instant;

// Using thread local to give access to all the methods (tcp_stream, tcp_listener etc..)
thread_local! {
    // Using RefCell to allow interior mutability
    pub static REACTOR_HANDLE: RefCell<Option<ReactorHandle>> = RefCell::new(None);
}

// The executor holds a HashMap of IDs to tasks
// And a vector of ready tasks IDs
pub struct Executor {
    tasks: HashMap<usize, Task>,
    ready_queue: Vec<usize>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            ready_queue: Vec::new(),
        }
    }

    // Spawn a new task and add it to the ready queue
    // The Task will be added to the ready queue by default because it is ready to be polled
    pub fn spawn(&mut self, task: impl Future<Output = ()> + 'static) {
        let id = self.tasks.len();
        self.tasks.insert(id, Task::new(task, id));
        self.ready_queue.push(id);
    }

    // Run Tasks until completion of them all
    pub fn block_on(mut self) {
        // Uses a channel to wake tasks
        // The waker send the task id through the channel
        // when the executor run out of tasks it consume the IDs
        let (tx, rx) = std::sync::mpsc::channel();
        let mut reactor = Reactor::new();
        // Initialze the reactor handle in the thread local
        REACTOR_HANDLE.with(|r| {
            *r.borrow_mut() = Some(ReactorHandle::new(&reactor));
        });

        // Looping the tasks
        loop {
            // Popping the ready queue to get the first task
            while let Some(task_id) = self.ready_queue.pop() {
                // Create a waker to this task, It takes the task id and a clone of sender
                // so it can send the task id through the channel
                let waker = MyWaker::new(task_id, tx.clone());
                let waker = Waker::from(Arc::new(waker));
                // Crafting a context with the waker to pass it to the task's poll method
                let cx = &mut std::task::Context::from_waker(&waker);
                let task = self.tasks.get_mut(&task_id).unwrap();
                let result = task.future.as_mut().poll(cx);
                match result {
                    std::task::Poll::Ready(()) => {
                        self.tasks.remove(&task_id);
                    }
                    std::task::Poll::Pending => (),
                }
            }
            // If there are no tasks left, break the loop
            if self.tasks.is_empty() {
                break;
            }
            // Wait for the reactor to notify us of ready tasks
            reactor.epoll();
            // Consume the channel to get ready tasks IDs
            while let Ok(task_id) = rx.try_recv() {
                self.ready_queue.push(task_id);
            }
        }
    }
}

// A struct representing a task to be executed by the runtime, it contains the future and the task ID
pub struct Task {
    // The future is pinned because futures might include self referential data
    pub future: Pin<Box<dyn Future<Output = ()>>>,
    // Task Id
    pub id: usize,
}
impl Task {
    pub fn new(task: impl Future<Output = ()> + 'static, id: usize) -> Self {
        Self {
            future: Box::pin(task),
            id,
        }
    }
}

// Waker struct that contains the task ID and a channel sender to notify the reactor of ready tasks
pub struct MyWaker {
    task_id: usize,
    tx: Sender<usize>,
}

impl MyWaker {
    fn new(id: usize, tx: Sender<usize>) -> Self {
        Self { task_id: id, tx }
    }
}

impl Wake for MyWaker {
    // Wake function on the waker, it sends the task ID to the reactor's channel to notify it of readiness
    fn wake(self: Arc<Self>) {
        self.tx.send(self.task_id).ok();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.tx.send(self.task_id).ok();
    }
}

// The Reactor struct that handles the event loop and notifies ready tasks to the runtime
pub struct Reactor {
    // Mio::Poll that handles the event loop and notifies ready tasks to the runtime
    pub poll: Poll,
    // Events that are ready to be processed
    pub events: Events,
    // Maps tokens to task IDs for quick lookup
    pub token_to_task_id: HashMap<Token, usize>,
    // The next token to be assigned to a task
    pub next_token: usize,
}
impl Reactor {
    pub fn new() -> Self {
        Self {
            poll: Poll::new().unwrap(),
            events: Events::with_capacity(1024),
            token_to_task_id: HashMap::new(),
            next_token: 0,
        }
    }

    // The function to wait until tasks are ready
    pub fn epoll(&mut self) {
        REACTOR_HANDLE.with(|r| {
            let mut handle_ref = r.borrow_mut();
            let handle = handle_ref.as_mut().unwrap();
            let mut shared = handle.shared.lock().unwrap();

            // Get the nearest timer and calculate the duration until it expires
            let timeout = handle
                .timers
                .peek()
                .map(|nearest| nearest.0.0.saturating_duration_since(Instant::now()));

            // Wait for events with timeout equal to the nearest timer
            // So if no events occur before the timer, the function stop so we can set the function with timer as ready
            let _ = self.poll.poll(&mut self.events, timeout);

            for event in self.events.iter() {
                let token = event.token();

                // Remove the waker from the shared state and wake it up
                if let Some(waker) = shared.waiters.remove(&token) {
                    waker.wake();
                }
            }

            let now = Instant::now();

            // Wake all the timers that have expired
            while let Some(nearest) = handle.timers.peek() {
                let deadline = nearest.0.0;

                if deadline > now {
                    break;
                }

                let (_, token) = handle.timers.pop().unwrap().0;

                if let Some(waker) = shared.waiters.remove(&token) {
                    waker.wake();
                }
            }
        });
    }
}

// The ReactorHandle struct that holds the registry and shared state for registering and waking tasks
pub struct ReactorHandle {
    // Mio Registry that register interest in OS events
    pub registry: mio::Registry,
    // Shared state that holds the next token and waiters for tasks
    pub shared: Arc<Mutex<ReactorState>>,
    // A heap for timers so we can get the min in O(1) time
    pub timers: BinaryHeap<std::cmp::Reverse<(std::time::Instant, mio::Token)>>,
}
pub struct ReactorState {
    // The next token to assign to a new registration
    pub next_token: usize,
    // A map of tokens to wakers for tasks that are waiting on events
    pub waiters: HashMap<mio::Token, std::task::Waker>,
}

impl ReactorHandle {
    pub fn new(reactor: &Reactor) -> Self {
        Self {
            registry: reactor.poll.registry().try_clone().unwrap(),
            shared: Arc::new(Mutex::new(ReactorState::new())),
            timers: BinaryHeap::new(),
        }
    }

    // A function to register a waker to an os event
    pub fn register<S>(
        &mut self,
        resource: &mut S,
        waker: std::task::Waker,
        interest: Interest,
        token: &mut Option<mio::Token>,
    ) where
        S: mio::event::Source + ?Sized,
    {
        let mut shared = self.shared.lock().unwrap();
        // if there isn't a token, create a new one and register it with the registry
        if token.is_none() {
            let created_token = Token(shared.next_token);
            *token = Some(created_token);

            shared.next_token += 1;

            let _ = self.registry.register(resource, created_token, interest);
            shared.waiters.insert(created_token, waker);
        } else { // if there is a token, reregister it with the registry
            if let Some(token) = token {
                let _ = self.registry.reregister(resource, *token, interest);
                shared.waiters.insert(*token, waker);
            }
        }
    }

    // A function to deregister a resource from the reactor
    pub fn deregister<S>(&mut self, resource: &mut S, token: &mut Option<mio::Token>)
    where
        S: mio::event::Source + ?Sized,
    {
        let mut shared = self.shared.lock().unwrap();
        if let Some(token) = token {
            shared.waiters.remove(token);
            let _ = self.registry.deregister(resource);
        }
        *token = None;
    }
}

impl ReactorState {
    pub fn new() -> Self {
        Self {
            next_token: 0,
            waiters: HashMap::new(),
        }
    }
}

// The module containing net types and implementations
pub mod net {
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::task;

    use mio::Interest;

    use crate::REACTOR_HANDLE;

    // a wrapper for TcpListener that registers itself with the reactor
    pub struct TcpListener {
        mio_tcp_listener: mio::net::TcpListener,
        token: Option<mio::Token>,
    }
    impl TcpListener {
        pub fn bind(addr: std::net::SocketAddr) -> Self {
            Self {
                mio_tcp_listener: mio::net::TcpListener::bind(addr).unwrap(),
                token: None,
            }
        }

        // asynchronously accepts a new connection
        pub fn accept(
            &mut self,
        ) -> impl std::future::Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + '_
        {
            // returns a PollFn
            std::future::poll_fn(|cx| match self.mio_tcp_listener.accept() {
                // If got a connection, deregister the listener and return the stream
                Ok((stream, addr)) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();
                        handle.deregister(&mut self.mio_tcp_listener, &mut self.token);
                    });
                    std::task::Poll::Ready(Ok((TcpStream::new(stream), addr)))
                }
                // If would block, register the listener and return pending
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.register(
                            &mut self.mio_tcp_listener,
                            cx.waker().clone(),
                            mio::Interest::READABLE,
                            &mut self.token,
                        );
                    });

                    std::task::Poll::Pending
                }
                Err(e) => std::task::Poll::Ready(Err(e)),
            })
        }
    }

    pub struct TcpStream {
        mio_tcp_stream: mio::net::TcpStream,
        // a buf for reading slipover bytes
        read_buf: Vec<u8>,
        token: Option<mio::Token>,
    }

    impl TcpStream {
        fn new(mio_tcp_stream: mio::net::TcpStream) -> Self {
            Self {
                mio_tcp_stream,
                read_buf: Vec::new(),
                token: None,
            }
        }
        fn poll_read(
            &mut self,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> task::Poll<std::io::Result<usize>> {
            match self.mio_tcp_stream.read(buf) {
                Ok(result) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();
                        handle.deregister(&mut self.mio_tcp_stream, &mut self.token);
                    });
                    task::Poll::Ready(Ok(result))
                }
                // If would block, register for read readiness and return Pending
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.register(
                            &mut self.mio_tcp_stream,
                            cx.waker().clone(),
                            mio::Interest::READABLE,
                            &mut self.token,
                        );
                    });
                    return task::Poll::Pending;
                }
                Err(e) => task::Poll::Ready(Err(e)),
            }
        }

        fn poll_read_line(
            &mut self,
            cx: &mut std::task::Context<'_>,
            buf: &mut String,
        ) -> task::Poll<std::io::Result<usize>> {
            buf.clear();

            // If there's a full line in the buffer, return it immediately
            if let Some(pos) = self.read_buf.iter().position(|&b| b == b'\n') {
                REACTOR_HANDLE.with(|r| {
                    let mut handle = r.borrow_mut();
                    let handle = handle.as_mut().unwrap();
                    handle.deregister(&mut self.mio_tcp_stream, &mut self.token);
                });
                buf.push_str(str::from_utf8(&self.read_buf[..=pos]).unwrap());
                self.read_buf.drain(..=pos);
                return task::Poll::Ready(Ok(pos + 1));
            }

            let mut temp_buf = [0u8; 1024];
            match self.mio_tcp_stream.read(&mut temp_buf) {
                // If the read returns 0, we've reached EOF, if there is data in the buffer, return it
                Ok(0) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();
                        handle.deregister(&mut self.mio_tcp_stream, &mut self.token);
                    });
                    buf.push_str(str::from_utf8(&self.read_buf).unwrap());
                    self.read_buf.clear();
                    return task::Poll::Ready(Ok(buf.len()));
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&temp_buf[..n]);

                    if let Some(pos) = self.read_buf.iter().position(|&b| b == b'\n') {
                        // Delete the bytes up to and including the newline
                        let _ = buf.push_str(str::from_utf8(&self.read_buf[..=pos]).unwrap());
                        let _ = self.read_buf.drain(..=pos).collect::<Vec<u8>>();
                        return task::Poll::Ready(Ok(pos + 1));
                    } else {
                        // No newline found, register for more data
                        REACTOR_HANDLE.with(|r| {
                            let mut handle = r.borrow_mut();
                            let handle = handle.as_mut().unwrap();

                            handle.register(
                                &mut self.mio_tcp_stream,
                                cx.waker().clone(),
                                Interest::READABLE,
                                &mut self.token,
                            );
                        });
                        task::Poll::Pending
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.register(
                            &mut self.mio_tcp_stream,
                            cx.waker().clone(),
                            Interest::READABLE,
                            &mut self.token,
                        );
                    });
                    task::Poll::Pending
                }
                Err(e) => task::Poll::Ready(Err(e)),
            }
        }

        pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_read(cx, buf)).await
        }

        pub async fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_read_line(cx, buf)).await
        }

        fn poll_write(
            &mut self,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> task::Poll<std::io::Result<usize>> {
            match self.mio_tcp_stream.write(buf) {
                Ok(n) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.deregister(&mut self.mio_tcp_stream, &mut self.token);
                    });
                    task::Poll::Ready(Ok(n))
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.register(
                            &mut self.mio_tcp_stream,
                            cx.waker().clone(),
                            mio::Interest::WRITABLE,
                            &mut self.token,
                        );
                    });
                    return task::Poll::Pending;
                }
                Err(e) => task::Poll::Ready(Err(e)),
            }
        }

        pub async fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_write(cx, buf)).await
        }
    }
}

// time module
pub mod time {
    use crate::REACTOR_HANDLE;
    use std::pin::Pin;
    use std::task;

    // Sleep struct that implements Future
    pub struct Sleep {
        deadline: std::time::Instant,
        registered: bool,
    }

    impl Future for Sleep {
        type Output = ();

        // implementing poll for sleep
        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> task::Poll<Self::Output> {
            // if the timer expired return ready
            if std::time::Instant::now() >= self.deadline {
                task::Poll::Ready(())
            } else {
                // if the timer still valid and isn't registered register it and return pending
                let waker = cx.waker().clone();
                if !self.registered {
                    self.registered = true;
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        let mut shared = handle.shared.lock().unwrap();
                        let token = shared.next_token;
                        shared.next_token += 1;
                        handle
                            .timers
                            .push(std::cmp::Reverse((self.deadline, mio::Token(token))));
                        shared.waiters.insert(mio::Token(token), waker);
                    });
                }
                task::Poll::Pending
            }
        }
    }

    // a constructor for sleep
    pub fn sleep(duration: std::time::Duration) -> Sleep {
        Sleep {
            deadline: std::time::Instant::now() + duration,
            registered: false,
        }
    }
}
