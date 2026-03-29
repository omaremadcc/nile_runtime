use std::net::SocketAddr;
use toy_runtime::time::sleep;
use toy_runtime::Executor;
use toy_runtime::net::TcpListener;

/// Example output:
/// ```console
/// $ cargo run --example tcp
/// Sleeping on 1
/// Sleeping on 2
/// Sleeping on 3
/// # (3 seconds pass)
/// Awake on 1
/// Awake on 2
/// Awake on 3
/// ```

fn main() {
    let mut executor = Executor::new();

    executor.spawn(fetch_some_data(1));
    executor.spawn(fetch_some_data(2));
    executor.spawn(fetch_some_data(3));
    executor.block_on();
}

async fn fetch_some_data(index: usize) {
    println!("Sleeping on {index}");
    sleep(std::time::Duration::from_secs(3)).await;
    println!("Awake on {index}");

    let mut tcp_listener =
        TcpListener::bind(format!("127.0.0.1:{}", 8080 + index).parse::<SocketAddr>().unwrap());

    let (mut stream, _addr) = tcp_listener.accept().await.unwrap();

    let mut request = String::new();
    loop {
        let n = stream.read_line(&mut request).await.unwrap();
        if n == 0 {
            break;
        }

        if request == "\r\n" || request == "\n" {
            break;
        }

        request.clear();
    }

    let response =
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nHello";

    stream.write(response).await.unwrap();
}
