// use mio::net::TcpListener;
use std::io::Read;
use std::net::SocketAddr;

use toy_runtime::Executor;
use toy_runtime::net::TcpListener;
fn main() {
    let mut executor = Executor::new();

    executor.spawn(fetch_some_data(1));
    executor.spawn(fetch_some_data(2));
    executor.spawn(fetch_some_data(3));
    executor.block_on();
    // let mut listener = TcpListener::bind("127.0.0.1:3000".parse::<SocketAddr>().unwrap()).unwrap();
    // loop {
    //     let res = listener.accept();
    //     match res {
    //         Ok((mut stream, addr)) => {
    //             let mut buffer = [0u8; 1024];
    //             let n = stream.read(&mut buffer);
    //             // println!("Received {} bytes from {}", n.unwrap(), addr);
    //         }
    //         Err(e) => {}
    //     }
    // }
}

async fn fetch_some_data(index: usize) {
    println!("We are fetching Some data {}", index);
    let mut tcp_listener =
        TcpListener::bind(format!("127.0.0.1:{index}").parse::<SocketAddr>().unwrap());

    let (mut stream, addr) = tcp_listener.accept().await.unwrap();
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer);
    match n {
        Ok(n) => {
            println!("Received {} bytes from {}", n, addr);
        }
        Err(e) => {
            println!("Error reading from stream: {}", e);
        }
    }
}
