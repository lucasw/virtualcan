use bytes::Bytes;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

pub fn run_server(port: u16) {
    let mut runtime = tokio::runtime::Builder::new()
        .basic_scheduler()
        .enable_all()
        .thread_name("Tokio-server-thread")
        .build()
        .unwrap();

    runtime.block_on(async {
        if let Err(err) = server_prog(port).await {
            error!("Server stopped with error: {}", err);
        }
    });
}

struct Server {
    peers: Vec<Peer>,
}

impl Server {
    fn new() -> Server {
        Server { peers: vec![] }
    }

    fn add_peer(&mut self, peer: Peer) {
        self.peers.push(peer);
    }

    fn broadcast(&mut self, source_port: u32, msg: Bytes) {
        for peer in &mut self.peers {
            peer.send_out(source_port, msg.clone());
        }
    }
}

struct Peer {
    tx: mpsc::UnboundedSender<Bytes>,

    /// A unique ID for this peers 'port'
    port_id: u32,
}

impl Peer {
    fn send_out(&mut self, source_port: u32, msg: Bytes) {
        trace!("Peer sendout msg {:?}", msg);
        if source_port != self.port_id {
            let _ = self.tx.unbounded_send(msg);
            // println!("unbounded port: {rv:?}");
        }
    }
}

enum ServerEvent {
    Message { source_port: u32, msg: Bytes },
    Peer(Peer),
}

async fn server_prog(port: u16) -> std::io::Result<()> {
    // let ip = std::net::Ipv6Addr::UNSPECIFIED;
    // let addr = std::net::SocketAddrV6::new(ip, port, 0, 0);
    let ip = std::net::Ipv4Addr::UNSPECIFIED;
    let addr = std::net::SocketAddrV4::new(ip, port);
    info!("Starting virtual can server at: {:?}", addr);
    let std_listener = std::net::TcpListener::bind(addr)?;
    let mut listener = TcpListener::from_std(std_listener)?;
    info!("Bound to {:?}", addr);

    let (broadcast_tx, distributor_rx) = mpsc::unbounded::<ServerEvent>();

    let _distributor_task_handle = tokio::spawn(async {
        let result = distributor_prog(distributor_rx).await;
        if let Err(err) = result {
            error!("Error in distribution task: {:?}", err);
        }
    });

    let mut peer_counter: u32 = 0;
    loop {
        let (client_socket, remote_addr) = listener.accept().await?;
        info!(
            "New socket from: {:?} --> id = {}",
            remote_addr, peer_counter
        );
        process_socket(broadcast_tx.clone(), client_socket, peer_counter);
        peer_counter += 1;
    }
}

async fn distributor_prog(mut rx: mpsc::UnboundedReceiver<ServerEvent>) -> std::io::Result<()> {
    let mut server = Server::new();
    while let Some(item) = rx.next().await {
        match item {
            ServerEvent::Message { source_port, msg } => {
                server.broadcast(source_port, msg);
            }
            ServerEvent::Peer(peer) => {
                server.add_peer(peer);
            }
        }
    }

    Ok(())
}

fn process_socket(
    broadcast_tx: mpsc::UnboundedSender<ServerEvent>,
    stream: TcpStream,
    peer_id: u32,
) {
    let _peer_task_handle = tokio::spawn(async move {
        let result = peer_prog(stream, broadcast_tx, peer_id).await;
        if let Err(err) = result {
            error!("Error in peer task: {:?}", err);
        }
    });
}

async fn peer_prog(
    mut stream: TcpStream,
    broadcast_tx: mpsc::UnboundedSender<ServerEvent>,
    peer_id: u32,
) -> std::io::Result<()> {
    // Create outbound channel:
    let (emit_tx, mut emit_rx) = mpsc::unbounded::<Bytes>();

    let peer = Peer {
        tx: emit_tx,
        port_id: peer_id,
    };
    broadcast_tx
        .unbounded_send(ServerEvent::Peer(peer))
        .unwrap();

    stream.set_nodelay(true)?;
    let (tcp_read, tcp_write) = stream.split();

    // Create packetizers:
    let mut packet_stream = FramedRead::new(tcp_read, LengthDelimitedCodec::new()).fuse();
    let mut packet_sink = FramedWrite::new(tcp_write, LengthDelimitedCodec::new());

    loop {
        futures::select! {
            optional_packet = packet_stream.next() => {
                if let Some(packet) = optional_packet {
                    let item = packet?.freeze();
                    trace!("Item! {:?}", item);
                    broadcast_tx
                    .unbounded_send(ServerEvent::Message { source_port: peer_id, msg: item } )
                    .unwrap();
                } else {
                    info!("No more incoming packets.");
                    break;
                }
            }
            optional_item = emit_rx.next() => {
                if let Some(item) = optional_item {
                    trace!("BROADCAST: {:?}", item);
                    packet_sink.send(item).await?;
                } else {
                    info!("No messages to broadcast.");
                    break;
                }
            }
        };
    }

    Ok(())
}
