// use mio::net::TcpListener;
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

    let (mut stream, _addr) = tcp_listener.accept().await.unwrap();
    // let mut buffer = [0u8; 4048];
    // let n = stream.read(&mut buffer).await;
    // match n {
    //     Ok(n) => {
    //         println!("Received {} bytes from {}", n, addr);
    //         println!("Received data: {:?}", str::from_utf8(&buffer).unwrap_or("").trim_end_matches("\0"));
    //     }
    //     Err(e) => {
    //         println!("Error reading from stream: {}", e);
    //     }
    // }
    let mut string = String::new();
    let _ = stream.read_line(&mut string).await;
    println!("Received Data: {}", string);

    while let Ok(n) = stream.read_line(&mut string).await && n > 0 {
        println!("Received Data: {}", string);
        string.clear();
    }


}
