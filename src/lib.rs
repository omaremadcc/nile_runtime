use mio::{Events, Interest, Poll, Token};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::task::{Wake, Waker};

thread_local! {
    pub static REACTOR_HANDLE: RefCell<Option<ReactorHandle>> = RefCell::new(None);
}
pub struct Executor {
    tasks: HashMap<usize, Task>,
    ready_queue: VecDeque<usize>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            ready_queue: VecDeque::new(),
        }
    }

    pub fn spawn(&mut self, task: impl Future<Output = ()> + 'static) {
        let id = self.tasks.len();
        self.tasks.insert(id, Task::new(task, id));
        self.ready_queue.push_back(id);
    }

    pub fn block_on(mut self) {
        println!("Started blocking");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut reactor = Reactor::new();
        REACTOR_HANDLE.with(|r| {
            *r.borrow_mut() = Some(ReactorHandle::new(&reactor));
        });

        loop {
            while let Some(task_id) = self.ready_queue.pop_front() {
                println!("running task {}", task_id);
                let waker = MyWaker::new(task_id, tx.clone());
                let waker = Waker::from(Arc::new(waker));
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
            if self.tasks.is_empty() {
                break;
            }
            reactor.epoll();
            while let Ok(task_id) = rx.try_recv() {
                self.ready_queue.push_back(task_id);
            }
        }
    }
}

pub struct Task {
    pub future: Pin<Box<dyn Future<Output = ()>>>,
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
    fn wake(self: Arc<Self>) {
        self.tx.send(self.task_id).ok();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.tx.send(self.task_id).ok();
    }
}

pub struct Reactor {
    pub poll: Poll,
    pub events: Events,
    pub token_to_task_id: HashMap<Token, usize>,
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

    pub fn epoll(&mut self) {
        let _ = self.poll.poll(&mut self.events, None);
        for event in self.events.iter() {
            println!("Running Epoll");
            let token = event.token();
            REACTOR_HANDLE.with(|r| {
                let mut handle = r.borrow_mut();
                let handle = handle.as_mut().unwrap();
                let waker = handle
                    .shared
                    .lock()
                    .unwrap()
                    .waiters
                    .remove(&token)
                    .unwrap();
                waker.wake();
            })
        }
    }
}

pub struct ReactorHandle {
    pub registry: mio::Registry,
    pub shared: Arc<Mutex<ReactorState>>,
}
pub struct ReactorState {
    pub next_token: usize,
    pub waiters: HashMap<mio::Token, std::task::Waker>,
}

impl ReactorHandle {
    pub fn new(reactor: &Reactor) -> Self {
        Self {
            registry: reactor.poll.registry().try_clone().unwrap(),
            shared: Arc::new(Mutex::new(ReactorState::new())),
        }
    }

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
        if token.is_none() {
            let created_token = Token(shared.next_token);
            *token = Some(created_token);

            shared.next_token += 1;

            let _ = self.registry.register(resource, created_token, interest);
            shared.waiters.insert(created_token, waker);
        } else {
            if let Some(token) = token {
                let _ = self.registry.reregister(resource, *token, interest);
               shared.waiters.insert(*token, waker);
            }
        }
    }
    pub fn deregister<S>(
        &mut self,
        resource: &mut S,
        token: &mut Option<mio::Token>,
    ) where
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

pub mod net {
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::task;

    use mio::Interest;

    use crate::REACTOR_HANDLE;

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

        pub fn accept(
            &mut self,
        ) -> impl std::future::Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + '_
        {
            println!("Running Accept");
            std::future::poll_fn(|cx| match self.mio_tcp_listener.accept() {
                Ok((stream, addr)) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();
                        handle.deregister(&mut self.mio_tcp_listener, &mut self.token);
                    });
                    std::task::Poll::Ready(Ok((TcpStream::new(stream), addr)))
                },
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
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    println!("Poll read will block");
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
                    // Add the new bytes to the buffer
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
            println!("Running Read");
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_read(cx, buf)).await
        }

        pub async fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            println!("reading a line");
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_read_line(cx, buf)).await
        }

        fn poll_write(&mut self, cx: &mut std::task::Context<'_>, buf: &[u8]) -> task::Poll<std::io::Result<usize>> {
            match self.mio_tcp_stream.write(buf) {
                Ok(n) => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.deregister(&mut self.mio_tcp_stream, &mut self.token);
                    });
                    task::Poll::Ready(Ok(n))
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE.with(|r| {
                        let mut handle = r.borrow_mut();
                        let handle = handle.as_mut().unwrap();

                        handle.register(&mut self.mio_tcp_stream, cx.waker().clone(), mio::Interest::WRITABLE, &mut self.token);
                    });
                    return task::Poll::Pending;
                },
                Err(e) => task::Poll::Ready(Err(e)),
            }
        }

        pub async fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            println!("Running Write");
            std::future::poll_fn(|cx| Pin::new(&mut *self).poll_write(cx, buf)).await
        }
    }
}

mod time {
    
}