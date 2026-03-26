use mio::{Events, Poll, Token};
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
        let _ = self
            .poll
            .poll(&mut self.events, Some(std::time::Duration::from_secs(0)));
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
    use std::net::SocketAddr;

    use crate::REACTOR_HANDLE;

    pub struct TcpListener {
        mio_tcp_listener: mio::net::TcpListener,
    }
    impl TcpListener {
        pub fn bind(addr: std::net::SocketAddr) -> Self {
            Self {
                mio_tcp_listener: mio::net::TcpListener::bind(addr).unwrap(),
            }
        }

        pub fn accept(
            &mut self,
        ) -> impl std::future::Future<Output = std::io::Result<(mio::net::TcpStream, SocketAddr)>> + '_
        {
            print!("Running Accept");
            std::future::poll_fn(|cx| match self.mio_tcp_listener.accept() {
                Ok((stream, addr)) => std::task::Poll::Ready(Ok((stream, addr))),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    REACTOR_HANDLE
                        .with(|r| {
                            let mut handle = r.borrow_mut();
                            let handle = handle.as_mut().unwrap();

                            let mut shared_state = handle.shared.lock().unwrap();
                            let token = mio::Token(shared_state.next_token);
                            shared_state.next_token += 1;

                            shared_state.waiters.insert(token, cx.waker().clone());

                            handle.registry.register(
                                &mut self.mio_tcp_listener,
                                token,
                                mio::Interest::READABLE,
                            )
                        })
                        .unwrap();

                    std::task::Poll::Pending
                }
                Err(e) => std::task::Poll::Ready(Err(e)),
            })
        }
    }
}
