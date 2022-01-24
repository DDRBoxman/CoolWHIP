use futures_util::{SinkExt, StreamExt, TryFutureExt};
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use std::borrow::BorrowMut;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender};
use tokio::sync::RwLock;
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

use crossfire::mpmc;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Deserialize, Serialize)]
struct WhipBody {
    desc: String,
}

type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Message>>>>;

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

#[tokio::main]
async fn main() {
    // Keep track of all connected users, key is usize, value
    // is a websocket sender.
    let users = Users::default();

    let (sdp_tx, sdp_rx) = mpmc::unbounded_future();

    let ws = warp::path("ws")
        // The `ws()` filter will prepare the Websocket handshake.
        .and(warp::ws())
        .and(with_users(users.clone()))
        .and(warp::any().map(move || sdp_tx.clone()))
        .map(
            |ws: warp::ws::Ws, users: Users, sdp_tx: mpmc::TxUnbounded<Message>| {
                // And then our closure will be called when it completes...
                ws.on_upgrade(move |websocket| websocket_connect(websocket, users.clone(), sdp_tx))
            },
        );

    let whip = warp::path("whip")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_users(users.clone()))
        .and(warp::any().map(move || sdp_rx.clone()))
        .and_then(handle_whip);

    let files = warp::get()
        .and(warp::fs::dir("./public/"));

    let routes = ws.or(whip).or(files);

    warp::serve(routes).run(([127, 0, 0, 1], 8000)).await;
}

fn with_users(
    users: Users,
) -> impl Filter<Extract = (Users,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || users.clone())
}

async fn handle_whip(
    whip: WhipBody,
    users: Users,
    sdp_rx: mpmc::RxUnbounded<Message>,
) -> Result<impl warp::Reply, std::convert::Infallible> {
    let message = json!(&whip);
    // New message from this user, send it to everyone else (except same uid)...
    for (&uid, tx) in users.read().await.iter() {
        if let Err(disconnected) = tx.send(Message::text(message.to_string())) {
            println!("{}", disconnected);
        }
    }

    println!("Waiting for SDP from WS");

    if let Ok(res) = sdp_rx.recv().await {
        println!("Got Client SDP");
        match res.to_str() {
            Ok(s) => {
                println!("returning sdp");
                return Ok(s.to_owned());
            },
            Err(e) => {
                println!("failed getting ws string");
            }
        }
    }

    println!("Returning test");

    Ok("test".to_string())
}

async fn websocket_connect(
    ws: warp::ws::WebSocket,
    users: Users,
    sdp_tx: mpmc::TxUnbounded<Message>,
) {
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    let (mut user_ws_tx, mut user_ws_rx) = ws.split();

    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {}", e);
                })
                .await;
        }
    });

    users.write().await.insert(my_id, tx);

    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error: {}", e);
                break;
            }
        };

        println!("Got Client SDP in WS");
        let res = sdp_tx.send(msg.clone());
        match res {
            Ok(m) => println!("sent"),
            Err(e) => println!("{}", e),
        }
    }
}
